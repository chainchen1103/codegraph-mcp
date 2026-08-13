//! 增量同步的整合測試：改了檔案之後，查詢立刻反映新的內容。

mod common;

use code_graph::store::Store;
use code_graph::{explore, sync};
use common::Fixture;

/// 同步一個檔案，並回傳同步之後的查詢結果。
trait Edit {
    fn edit(&self, rel_path: &str, body: &str);
    fn sync(&self, rel_path: &str) -> sync::SyncReport;
    fn ask(&self, input: &str) -> explore::Exploration;
}

impl Edit for Fixture {
    fn edit(&self, rel_path: &str, body: &str) {
        self.write(rel_path, body);
    }

    fn sync(&self, rel_path: &str) -> sync::SyncReport {
        let mut store = Store::open(&self.project.db_path()).unwrap();
        sync::file(&self.project, &mut store, rel_path).unwrap().1
    }

    fn ask(&self, input: &str) -> explore::Exploration {
        explore::explore(&self.project, &self.store(), input).unwrap()
    }
}

#[test]
fn a_renamed_function_is_visible_to_the_next_query() {
    let f = Fixture::indexed("sync-rename", &[("src/a.rs", "pub fn before() {}\n")]);

    f.edit("src/a.rs", "pub fn after() {}\n");
    f.sync("src/a.rs");

    let found = f.ask("after");
    assert!(!found.is_empty(), "{}", found.text);
    assert!(f.ask("before").is_empty(), "舊名字還查得到");
}

/// 先寫呼叫端、後寫被呼叫端是常見的編輯順序，第二次存檔要把邊補上。
#[test]
fn writing_the_caller_before_the_callee_still_produces_an_edge() {
    let f = Fixture::indexed("sync-order", &[("src/lib.rs", "pub fn root() {}\n")]);

    f.edit("src/caller.rs", "pub fn caller() {\n    callee();\n}\n");
    f.sync("src/caller.rs");
    assert_eq!(edge_count(&f), 0, "目標還不存在就先接了邊");

    f.edit("src/callee.rs", "pub fn callee() {}\n");
    let report = f.sync("src/callee.rs");

    assert_eq!(report.requeued, 1, "{report:?}");
    assert_eq!(edge_count(&f), 1, "第二次存檔之後邊沒有補上");

    let flow = f.ask("caller callee");
    assert!(flow.text.contains("## Flow"), "{}", flow.text);
}

#[test]
fn a_deleted_file_disappears_from_queries() {
    let f = Fixture::indexed(
        "sync-delete",
        &[
            ("src/a.rs", "pub fn kept() {}\n"),
            ("src/b.rs", "pub fn gone() {}\n"),
        ],
    );

    std::fs::remove_file(f.project.root().join("src/b.rs")).unwrap();
    f.sync("src/b.rs");

    assert!(f.ask("gone").is_empty());
    assert!(!f.ask("kept").is_empty());
}

/// 自然語言查詢走全文檢索，增量寫入靠 trigger 跟上，沒有 rebuild 這一步。
#[test]
fn documentation_changes_reach_full_text_search() {
    let f = Fixture::indexed(
        "sync-fts",
        &[("src/a.rs", "/// 舊的說明\npub fn thing() {}\n")],
    );

    f.edit("src/a.rs", "/// 全新的說明\npub fn thing() {}\n");
    f.sync("src/a.rs");

    let found = f.ask("全新的說明");
    assert!(
        found.selection.hits.iter().any(|h| h.qualified == "thing"),
        "{}",
        found.text
    );
}

/// 同步只做該做的事：沒有變過的檔案不重寫，識別碼因此保持穩定。
#[test]
fn syncing_twice_changes_nothing_the_second_time() {
    let f = Fixture::indexed(
        "sync-stable",
        &[("src/a.rs", "pub fn one() {}\npub fn two() {}\n")],
    );

    let ids = |f: &Fixture| -> Vec<(i64, String)> {
        let store = f.store();
        let mut stmt = store
            .conn()
            .prepare("SELECT id, qualified FROM symbols ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };

    let before = ids(&f);
    f.edit(
        "src/a.rs",
        "pub fn one() {}\npub fn two() {\n    one();\n}\n",
    );
    f.sync("src/a.rs");

    let after = ids(&f);
    assert_eq!(
        before.iter().map(|(_, n)| n).collect::<Vec<_>>(),
        after.iter().map(|(_, n)| n).collect::<Vec<_>>()
    );
    assert_eq!(
        before.iter().map(|(i, _)| i).collect::<Vec<_>>(),
        after.iter().map(|(i, _)| i).collect::<Vec<_>>(),
        "簽名沒變的符號換了識別碼，既有的邊會全部失效"
    );
}

fn edge_count(f: &Fixture) -> i64 {
    f.store()
        .conn()
        .query_row("SELECT count(*) FROM relations", [], |r| r.get(0))
        .unwrap()
}
