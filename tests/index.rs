//! 全量索引的整合測試：走訪、抽取、寫入三層串起來的行為。

mod common;

use code_graph::store::Store;
use common::{Fixture, query_one};

#[test]
fn a_small_project_is_indexed_end_to_end() {
    let f = Fixture::new("index-small", &[]);
    f.write(
        "src/store.rs",
        "/// 開啟資料庫\npub struct Store;\n\nimpl Store {\n    pub fn open() -> Store {\n        Store\n    }\n}\n",
    );
    f.write("src/util.rs", "pub fn helper() {}\n");
    f.write("README.md", "# 說明\n");

    let (report, store) = f.index();

    assert_eq!(report.files, 2, "只有原始碼檔案該被索引");
    assert_eq!(report.symbols, 3);
    assert!(report.warnings.is_empty());

    // 限定名與種類都要寫進去。
    let kind: i64 = query_one(&store, "SELECT kind FROM symbols WHERE name = 'open'");
    assert_eq!(kind, code_graph::Kind::Method as i64);

    // 文件註解跟著符號一起存。
    let doc: String = query_one(&store, "SELECT docstring FROM symbols WHERE name = 'Store'");
    assert_eq!(doc, "開啟資料庫");
}

#[test]
fn every_symbol_is_reachable_through_full_text_search() {
    let f = Fixture::new("index-fts", &[]);
    f.write(
        "src/a.rs",
        "/// 建立索引\npub fn build_index(root: &str) -> usize {\n    0\n}\n",
    );

    let (_, store) = f.index();

    for query in ["build_index", "建立索引", "usize"] {
        let hits: i64 = query_one(
            &store,
            &format!("SELECT count(*) FROM symbols_fts WHERE symbols_fts MATCH '{query}'"),
        );
        assert!(hits >= 1, "FTS 查不到 `{query}`");
    }
}

#[test]
fn file_rows_carry_the_path_and_content_hash() {
    let f = Fixture::new("index-files", &[]);
    f.write("src/a.rs", "fn a() {}\n");

    let (_, store) = f.index();

    let path: String = query_one(&store, "SELECT path FROM files");
    assert_eq!(path, "src/a.rs", "路徑要相對專案根目錄且以斜線分隔");

    let hash: String = query_one(&store, "SELECT content_hash FROM files");
    assert_eq!(hash.len(), 32);

    let indexed_at: i64 = query_one(&store, "SELECT indexed_at FROM files");
    assert!(indexed_at > 1_577_836_800_000);
}

#[test]
fn every_symbol_has_a_unique_handle() {
    let f = Fixture::new("index-handles", &[]);
    for i in 0..40 {
        f.write(&format!("src/m{i}.rs"), &format!("fn f{i}() {{}}\n"));
    }

    let (report, store) = f.index();
    assert_eq!(report.symbols, 40);

    let distinct: i64 = query_one(&store, "SELECT count(DISTINCT handle) FROM monikers");
    assert_eq!(distinct, 40, "短碼出現重複");

    let total: i64 = query_one(&store, "SELECT count(*) FROM monikers");
    assert_eq!(total, 40);
}

#[test]
fn editing_a_file_updates_its_symbols() {
    let f = Fixture::new("index-edit", &[]);
    f.write("src/a.rs", "fn before() {}\n");

    {
        let (_, store) = f.index();
        let name: String = query_one(&store, "SELECT name FROM symbols");
        assert_eq!(name, "before");
    }

    f.write("src/a.rs", "fn after() {}\nfn extra() {}\n");
    let (report, store) = f.index();

    assert_eq!(report.symbols, 2);
    let names: i64 = query_one(&store, "SELECT count(*) FROM symbols WHERE name = 'before'");
    assert_eq!(names, 0, "舊符號沒有被清掉");
}

#[test]
fn gitignored_directories_never_reach_the_index() {
    let f = Fixture::new("index-ignored", &[]);
    f.write(".gitignore", "vendor/\n");
    f.write("src/a.rs", "fn mine() {}\n");
    f.write("vendor/lib.rs", "fn theirs() {}\n");

    let (report, store) = f.index();

    assert_eq!(report.files, 1);
    let theirs: i64 = query_one(&store, "SELECT count(*) FROM symbols WHERE name = 'theirs'");
    assert_eq!(theirs, 0);
}

#[test]
fn test_files_are_flagged_so_they_can_be_filtered_later() {
    let f = Fixture::new("index-testflag", &[]);
    f.write("src/a.rs", "fn production() {}\n");
    f.write("tests/a.rs", "fn checks() {}\n");

    let (_, store) = f.index();

    let flagged: String = query_one(&store, "SELECT path FROM files WHERE is_test = 1");
    assert_eq!(flagged, "tests/a.rs");
}

/// 索引結果與檔案系統的列舉順序無關，兩次索引的識別碼必須一致。
#[test]
fn two_runs_produce_the_same_identifiers() {
    let f = Fixture::new("index-stable", &[]);
    for name in ["c", "a", "b"] {
        f.write(&format!("src/{name}.rs"), &format!("fn {name}() {{}}\n"));
    }

    let collect = |store: &Store| -> Vec<(i64, String)> {
        let mut stmt = store
            .conn()
            .prepare("SELECT id, moniker FROM monikers ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };

    let (_, store) = f.index();
    let first = collect(&store);
    drop(store);

    let (_, store) = f.index();
    assert_eq!(first, collect(&store));

    // 依路徑排序，與建立檔案的順序無關。
    assert_eq!(
        first
            .iter()
            .map(|(_, m)| m.as_str())
            .collect::<Vec<_>>()
            .first()
            .copied(),
        Some("src/a.rs:function:a:1")
    );
}

#[test]
fn symbols_never_outlive_the_file_they_came_from() {
    let f = Fixture::new("index-cascade", &[]);
    f.write("src/a.rs", "fn a() {}\n");
    f.write("src/b.rs", "fn b() {}\n");

    let (_, store) = f.index();

    store
        .conn()
        .execute("DELETE FROM files WHERE path = 'src/b.rs'", [])
        .unwrap();

    let left: i64 = query_one(&store, "SELECT count(*) FROM symbols");
    assert_eq!(left, 1, "刪除檔案沒有連帶清掉它的符號");
}
