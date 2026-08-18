//! 增量同步：只重新解析變更過的檔案。
//!
//! 全量索引清空整份索引再重建；這裡相反，把單一檔案的舊內容換掉，其餘
//! 部分原封不動。單檔的替換必須是原子的——「舊的刪了、新的沒進」會讓
//! 查詢安靜地少回結果，而不會報錯。

pub mod watch;

use std::time::{Duration, Instant};

use rusqlite::Connection;

use crate::error::Result;
use crate::extract;
use crate::project::{Project, unit, walk};
use crate::resolve;
use crate::store::write::{FileMeta, Writer};
use crate::store::{Store, content_hash};

/// 單一檔案的同步結果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// 內容與索引裡的一致，什麼都沒做。
    Unchanged,
    /// 重新解析並寫入。
    Updated,
    /// 檔案已不存在，索引裡的內容被移除。
    Removed,
    /// 副檔名不支援或內容讀不進來，不納入索引。
    Skipped,
}

/// 一次同步的結果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub updated: usize,
    pub removed: usize,
    pub unchanged: usize,
    /// 新建立的邊數。
    pub resolved: usize,
    /// 由先前失敗的引用重新撿回來、重新排入解析的引用數。
    pub requeued: usize,
    pub warnings: Vec<String>,
    pub elapsed: Duration,
}

/// 同步單一檔案。
///
/// `rel_path` 相對專案根目錄。刪除舊內容、寫入新內容與解析引用都在同一
/// 個交易裡完成。
pub fn file(project: &Project, store: &mut Store, rel_path: &str) -> Result<(Outcome, SyncReport)> {
    let started = Instant::now();
    let mut report = SyncReport::default();

    let outcome = sync_one(project, store, rel_path, &mut report)?;
    count(&mut report, outcome);

    report.elapsed = started.elapsed();
    Ok((outcome, report))
}

/// 掃過整個專案，同步所有與索引不一致的檔案。
///
/// 內容雜湊相同的檔案直接跳過，因此重跑的成本與變更量成正比，而不是與
/// 專案大小成正比。
pub fn project(project: &Project, store: &mut Store) -> Result<SyncReport> {
    let started = Instant::now();
    let mut report = SyncReport::default();

    for file in walk::source_files(project) {
        let outcome = sync_one(project, store, &file.rel_path, &mut report)?;
        count(&mut report, outcome);
    }

    for gone in vanished_files(project, store)? {
        remove(store, &gone)?;
        report.removed += 1;
    }

    store.set_metadata("indexed_at", &crate::store::now_millis().to_string())?;
    report.elapsed = started.elapsed();
    Ok(report)
}

fn count(report: &mut SyncReport, outcome: Outcome) {
    match outcome {
        Outcome::Updated => report.updated += 1,
        Outcome::Removed => report.removed += 1,
        Outcome::Unchanged => report.unchanged += 1,
        Outcome::Skipped => {}
    }
}

/// 索引裡有、磁碟上已經沒有的檔案。
fn vanished_files(project: &Project, store: &Store) -> Result<Vec<String>> {
    let mut stmt = store
        .conn()
        .prepare("SELECT path FROM files ORDER BY path")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;

    let mut gone = Vec::new();
    for row in rows {
        let path = row?;
        if !project.root().join(&path).is_file() {
            gone.push(path);
        }
    }
    Ok(gone)
}

/// 同步一個檔案，計數交給呼叫端。
fn sync_one(
    project: &Project,
    store: &mut Store,
    rel_path: &str,
    report: &mut SyncReport,
) -> Result<Outcome> {
    let absolute = project.root().join(rel_path);

    if !absolute.is_file() {
        remove(store, rel_path)?;
        return Ok(Outcome::Removed);
    }

    let bytes = match std::fs::read(&absolute) {
        Ok(b) => b,
        Err(e) => {
            report.warnings.push(format!("{rel_path}：讀取失敗，{e}"));
            return Ok(Outcome::Skipped);
        }
    };
    let Ok(source) = String::from_utf8(bytes) else {
        report
            .warnings
            .push(format!("{rel_path}：不是 UTF-8 文字檔"));
        return Ok(Outcome::Skipped);
    };

    let hash = content_hash(source.as_bytes());
    if recorded_hash(store.conn(), rel_path)?.as_deref() == Some(hash.as_str()) {
        return Ok(Outcome::Unchanged);
    }

    let Some(parse) = extract::extract(rel_path, &source) else {
        return Ok(Outcome::Skipped);
    };
    report.warnings.extend(parse.errors.iter().cloned());

    let module = crate::project::module_path(rel_path);
    let (resolved, requeued) = store.with_transaction(|conn| {
        let old = symbol_ids_of(conn, rel_path)?;
        clear_file(conn, rel_path)?;

        let mut writer = Writer::resume(conn)?;
        let unit = writer.unit(conn, &unit::of(project.root(), rel_path))?;
        let language = crate::project::language_of(rel_path);
        writer.write_file(
            conn,
            unit,
            FileMeta {
                rel_path,
                module_path: &module,
                language,
                content_hash: &hash,
            },
            &parse,
        )?;

        drop_edges_into_vanished(conn, rel_path, &old)?;

        // 這個檔案新提供的名字，可能正是別處等著的那一個。
        let names: Vec<String> = parse.symbols.iter().map(|s| s.name.clone()).collect();
        // 這個檔案的 import 剛換過一批，先接上再解析引用。
        resolve::imports::link(conn)?;
        let requeued = resolve::requeue_by_names(conn, &names)?;
        // 這條路徑只寫了一個檔案，索引裡查不到的名字可能只是還沒輪到。
        let resolved = resolve::resolve_pending(conn, resolve::Unknown::Keep)?.resolved;

        Ok((resolved, requeued))
    })?;

    report.resolved += resolved;
    report.requeued += requeued;
    Ok(Outcome::Updated)
}

/// 把一個檔案從索引裡整個移除。
fn remove(store: &mut Store, rel_path: &str) -> Result<()> {
    store.with_transaction(|conn| {
        let old = symbol_ids_of(conn, rel_path)?;
        clear_file(conn, rel_path)?;
        delete_edges_to(conn, &old)?;
        conn.execute("DELETE FROM files WHERE path = ?1", [rel_path])?;
        Ok(())
    })
}

/// 一個檔案在索引裡的識別碼，不在索引裡時回 `None`。
fn file_id_of(conn: &Connection, rel_path: &str) -> Result<Option<i64>> {
    let mut stmt = conn.prepare_cached("SELECT id FROM files WHERE path = ?1")?;
    let mut rows = stmt.query([rel_path])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

/// 一個檔案目前在索引裡的符號識別碼。
fn symbol_ids_of(conn: &Connection, rel_path: &str) -> Result<Vec<i64>> {
    let Some(file_id) = file_id_of(conn, rel_path)? else {
        return Ok(Vec::new());
    };

    let mut stmt = conn.prepare_cached("SELECT id FROM symbols WHERE file_id = ?1 ORDER BY id")?;
    let rows = stmt.query_map([file_id], |r| r.get::<_, i64>(0))?;

    let mut ids = Vec::new();
    for row in rows {
        ids.push(row?);
    }
    Ok(ids)
}

/// 清掉一個檔案既有的符號、引用與它發出的邊。
///
/// 指向這些符號的入邊先留著：改寫之後仍然存在的符號會拿回同一個識別
/// 碼，它們的入邊本來就還是對的。
fn clear_file(conn: &Connection, rel_path: &str) -> Result<()> {
    let Some(file_id) = file_id_of(conn, rel_path)? else {
        return Ok(());
    };

    conn.execute(
        "DELETE FROM relations WHERE src IN (SELECT id FROM symbols WHERE file_id = ?1)",
        [file_id],
    )?;
    conn.execute("DELETE FROM unresolved_refs WHERE file_id = ?1", [file_id])?;
    // 這個檔案的 import 隨著重新解析整批換掉；別的檔案指向它的那些
    // （target_id）留著，它們的目標仍然是同一個檔案。
    conn.execute("DELETE FROM imports WHERE file_id = ?1", [file_id])?;
    conn.execute("DELETE FROM symbols WHERE file_id = ?1", [file_id])?;
    Ok(())
}

/// 刪掉指向已消失符號的入邊。
///
/// 改寫之後還在的符號保有原本的識別碼，入邊照樣成立；真的消失的那些，
/// 留著入邊就是一條指向不存在節點的邊。
fn drop_edges_into_vanished(conn: &Connection, rel_path: &str, old: &[i64]) -> Result<()> {
    let survivors = symbol_ids_of(conn, rel_path)?;
    let vanished: Vec<i64> = old
        .iter()
        .copied()
        .filter(|id| !survivors.contains(id))
        .collect();
    delete_edges_to(conn, &vanished)
}

/// 刪掉指向這些符號的所有邊。
fn delete_edges_to(conn: &Connection, ids: &[i64]) -> Result<()> {
    let mut stmt = conn.prepare_cached("DELETE FROM relations WHERE dst = ?1")?;
    for id in ids {
        stmt.execute([id])?;
    }
    Ok(())
}

/// 索引裡記錄的內容雜湊，檔案不在索引裡時回 `None`。
fn recorded_hash(conn: &Connection, rel_path: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare_cached("SELECT content_hash FROM files WHERE path = ?1")?;
    let mut rows = stmt.query([rel_path])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{cleanup, indexed_project, write};

    /// 已索引的專案加上一個可重複呼叫的同步入口。
    struct Fixture {
        project: Project,
        store: Store,
    }

    impl Fixture {
        fn new(tag: &str, files: &[(&str, &str)]) -> Self {
            let project = indexed_project(&format!("sync-{tag}"), files);
            let store = Store::open(&project.db_path()).unwrap();
            Self { project, store }
        }

        fn write(&self, rel_path: &str, body: &str) {
            write(&self.project, rel_path, body);
        }

        fn sync(&mut self, rel_path: &str) -> Outcome {
            file(&self.project, &mut self.store, rel_path).unwrap().0
        }

        fn sync_all(&mut self) -> SyncReport {
            project(&self.project, &mut self.store).unwrap()
        }

        /// 所有的邊，以限定名表示。
        fn edges(&self) -> Vec<(String, String)> {
            let mut stmt = self
                .store
                .conn()
                .prepare(
                    "SELECT a.qualified, b.qualified FROM relations r
                     JOIN symbols a ON a.id = r.src
                     JOIN symbols b ON b.id = r.dst
                     ORDER BY a.qualified, b.qualified",
                )
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
        }

        fn names(&self) -> Vec<String> {
            let mut stmt = self
                .store
                .conn()
                .prepare("SELECT qualified FROM symbols ORDER BY qualified")
                .unwrap();
            stmt.query_map([], |r| r.get(0))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            cleanup(&self.project);
        }
    }

    #[test]
    fn an_unchanged_file_is_left_alone() {
        let mut f = Fixture::new("unchanged", &[("src/a.rs", "fn one() {}\n")]);
        assert_eq!(f.sync("src/a.rs"), Outcome::Unchanged);
    }

    #[test]
    fn a_renamed_symbol_replaces_the_old_one() {
        let mut f = Fixture::new("rename", &[("src/a.rs", "fn before() {}\n")]);
        f.write("src/a.rs", "fn after() {}\n");

        assert_eq!(f.sync("src/a.rs"), Outcome::Updated);
        assert_eq!(f.names(), vec!["after"], "舊符號還在索引裡");
    }

    #[test]
    fn a_new_file_is_added_without_touching_the_rest() {
        let mut f = Fixture::new("added", &[("src/a.rs", "fn one() {}\n")]);
        f.write("src/b.rs", "fn two() {}\n");

        assert_eq!(f.sync("src/b.rs"), Outcome::Updated);
        assert_eq!(f.names(), vec!["one", "two"]);
    }

    #[test]
    fn a_deleted_file_takes_its_symbols_with_it() {
        let mut f = Fixture::new(
            "deleted",
            &[("src/a.rs", "fn one() {}\n"), ("src/b.rs", "fn two() {}\n")],
        );
        std::fs::remove_file(f.project.root().join("src/b.rs")).unwrap();

        assert_eq!(f.sync("src/b.rs"), Outcome::Removed);
        assert_eq!(f.names(), vec!["one"]);
        assert_eq!(f.store.stats().unwrap().files, 1);
    }

    /// 增量寫入接在既有索引之後，識別碼不得與既有的符號相撞。
    #[test]
    fn new_symbols_get_ids_of_their_own() {
        let mut f = Fixture::new("ids", &[("src/a.rs", "fn one() {}\n")]);
        f.write("src/b.rs", "fn two() {}\nfn three() {}\n");
        f.sync("src/b.rs");

        let distinct: i64 = f
            .store
            .conn()
            .query_row("SELECT count(DISTINCT id) FROM symbols", [], |r| r.get(0))
            .unwrap();
        assert_eq!(distinct, 3, "有符號被蓋掉了");

        let orphans: i64 = f
            .store
            .conn()
            .query_row(
                "SELECT count(*) FROM symbols WHERE id NOT IN (SELECT id FROM monikers)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphans, 0);
    }

    #[test]
    fn handles_stay_unique_across_incremental_writes() {
        let mut f = Fixture::new("handles", &[("src/a.rs", "fn one() {}\n")]);
        f.write("src/b.rs", "fn two() {}\n");
        f.sync("src/b.rs");

        let (rows, distinct): (i64, i64) = f
            .store
            .conn()
            .query_row(
                "SELECT count(*), count(DISTINCT handle) FROM monikers",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(rows, distinct, "短碼重複了");
    }

    /// 改一個檔案的內容，指向它的邊不該因此消失。
    #[test]
    fn edges_into_a_changed_file_survive() {
        let mut f = Fixture::new(
            "inbound",
            &[
                ("src/a.rs", "pub fn target() {}\n"),
                ("src/b.rs", "fn caller() {\n    target();\n}\n"),
            ],
        );
        assert_eq!(
            f.edges(),
            vec![("caller".to_string(), "target".to_string())]
        );

        f.write("src/a.rs", "pub fn target() {\n    let x = 1;\n}\n");
        f.sync("src/a.rs");

        assert_eq!(
            f.edges(),
            vec![("caller".to_string(), "target".to_string())],
            "符號還在，入邊卻不見了"
        );
    }

    /// 被呼叫的符號消失時，指向它的邊也要消失。
    #[test]
    fn edges_into_a_vanished_symbol_are_dropped() {
        let mut f = Fixture::new(
            "vanished",
            &[
                ("src/a.rs", "pub fn target() {}\n"),
                ("src/b.rs", "fn caller() {\n    target();\n}\n"),
            ],
        );
        assert_eq!(f.edges().len(), 1);

        f.write("src/a.rs", "pub fn something_else() {}\n");
        f.sync("src/a.rs");

        assert!(
            f.edges().is_empty(),
            "留下了指向不存在符號的邊：{:?}",
            f.edges()
        );
    }

    /// 先寫呼叫端、後寫被呼叫端，第二次存檔要把邊補上。
    #[test]
    fn an_edge_appears_once_the_callee_is_written() {
        let mut f = Fixture::new("later", &[("src/a.rs", "fn placeholder() {}\n")]);

        f.write("src/b.rs", "fn caller() {\n    not_yet();\n}\n");
        f.sync("src/b.rs");
        assert!(f.edges().is_empty(), "目標還不存在就先接了邊");

        f.write("src/c.rs", "pub fn not_yet() {}\n");
        let (_, report) = file(&f.project, &mut f.store, "src/c.rs").unwrap();

        assert_eq!(report.requeued, 1, "失敗的引用沒有被撿回來");
        assert_eq!(
            f.edges(),
            vec![("caller".to_string(), "not_yet".to_string())]
        );
    }

    /// 同一個檔案來回改，邊的數量不能累積。
    #[test]
    fn repeated_edits_do_not_accumulate_edges() {
        let mut f = Fixture::new(
            "repeat",
            &[
                ("src/a.rs", "pub fn target() {}\n"),
                ("src/b.rs", "fn caller() {\n    target();\n}\n"),
            ],
        );

        for i in 0..3 {
            f.write(
                "src/b.rs",
                &format!("fn caller() {{\n    target();\n    let x = {i};\n}}\n"),
            );
            f.sync("src/b.rs");
            assert_eq!(
                f.edges().len(),
                1,
                "第 {i} 次改寫之後邊變成 {:?}",
                f.edges()
            );
        }
    }

    /// 全文檢索靠 trigger 跟上增量寫入，這條路徑沒有 rebuild 這一步。
    #[test]
    fn full_text_search_follows_incremental_writes() {
        let mut f = Fixture::new("fts", &[("src/a.rs", "fn before() {}\n")]);
        f.write("src/a.rs", "fn after_the_change() {}\n");
        f.sync("src/a.rs");

        let hits = |term: &str| -> i64 {
            f.store
                .conn()
                .query_row(
                    "SELECT count(*) FROM symbols_fts WHERE symbols_fts MATCH ?1",
                    [term],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!(hits("after_the_change"), 1, "新符號沒有進到全文檢索");
        assert_eq!(hits("before"), 0, "舊符號還留在全文檢索裡");
    }

    #[test]
    fn a_project_wide_sync_picks_up_every_kind_of_change() {
        let mut f = Fixture::new(
            "project",
            &[
                ("src/a.rs", "fn one() {}\n"),
                ("src/b.rs", "fn two() {}\n"),
                ("src/c.rs", "fn three() {}\n"),
            ],
        );

        f.write("src/a.rs", "fn one_renamed() {}\n");
        f.write("src/d.rs", "fn four() {}\n");
        std::fs::remove_file(f.project.root().join("src/c.rs")).unwrap();

        let report = f.sync_all();
        assert_eq!(report.updated, 2, "{report:?}");
        assert_eq!(report.removed, 1, "{report:?}");
        assert_eq!(report.unchanged, 1, "{report:?}");
        assert_eq!(f.names(), vec!["four", "one_renamed", "two"]);
    }

    #[test]
    fn a_project_wide_sync_of_an_untouched_project_does_nothing() {
        let mut f = Fixture::new(
            "noop",
            &[("src/a.rs", "fn one() {}\n"), ("src/b.rs", "fn two() {}\n")],
        );

        let report = f.sync_all();
        assert_eq!(report.updated, 0);
        assert_eq!(report.removed, 0);
        assert_eq!(report.unchanged, 2);
    }

    #[test]
    fn a_file_that_is_not_source_is_skipped() {
        let mut f = Fixture::new("skip", &[("src/a.rs", "fn one() {}\n")]);
        f.write("notes.md", "# 標題\n");

        assert_eq!(f.sync("notes.md"), Outcome::Skipped);
    }

    #[test]
    fn a_binary_file_is_reported_and_skipped() {
        let mut f = Fixture::new("binary", &[("src/a.rs", "fn one() {}\n")]);
        std::fs::write(f.project.root().join("src/b.rs"), [0xff, 0xfe, 0x00]).unwrap();

        let (outcome, report) = file(&f.project, &mut f.store, "src/b.rs").unwrap();
        assert_eq!(outcome, Outcome::Skipped);
        assert!(
            report.warnings.iter().any(|w| w.contains("UTF-8")),
            "{report:?}"
        );
    }

    #[test]
    fn syntax_errors_are_reported_but_the_file_is_still_indexed() {
        let mut f = Fixture::new("broken", &[("src/a.rs", "fn one() {}\n")]);
        f.write("src/a.rs", "fn still_here() {}\nfn broken( {\n");

        let (outcome, report) = file(&f.project, &mut f.store, "src/a.rs").unwrap();
        assert_eq!(outcome, Outcome::Updated);
        assert!(!report.warnings.is_empty());
        assert!(f.names().contains(&"still_here".to_string()));
    }

    /// 替換必須是原子的。中途失敗留下「舊的刪了、新的沒進」的狀態時，
    /// 查詢會安靜地少回結果，而且不會有任何錯誤提示。
    #[test]
    fn an_interrupted_replacement_leaves_the_old_content_intact() {
        let mut f = Fixture::new(
            "atomic",
            &[
                ("src/a.rs", "pub fn target() {}\n"),
                ("src/b.rs", "fn caller() {\n    target();\n}\n"),
            ],
        );
        let before = (f.names(), f.edges());

        let outcome: Result<()> = f.store.with_transaction(|conn| {
            let old = symbol_ids_of(conn, "src/a.rs")?;
            clear_file(conn, "src/a.rs")?;
            delete_edges_to(conn, &old)?;
            Err(crate::error::CgError::Corrupt {
                detail: "中途失敗".into(),
            })
        });

        assert!(outcome.is_err());
        assert_eq!((f.names(), f.edges()), before, "交易回滾之後索引不一致");
    }

    /// 單檔增量的成本要與檔案大小相關，而不是與專案大小相關。
    #[test]
    fn a_single_file_sync_stays_well_under_the_budget() {
        let files: Vec<(String, String)> = (0..200)
            .map(|i| {
                (
                    format!("src/f{i}.rs"),
                    format!("pub fn f{i}() {{\n    f{}();\n}}\n", (i + 1) % 200),
                )
            })
            .collect();
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_str()))
            .collect();

        let mut f = Fixture::new("budget", &borrowed);
        f.write("src/f7.rs", "pub fn f7() {\n    f8();\n    let x = 1;\n}\n");

        let (outcome, report) = file(&f.project, &mut f.store, "src/f7.rs").unwrap();
        assert_eq!(outcome, Outcome::Updated);
        assert!(
            report.elapsed < Duration::from_millis(200),
            "單檔增量花了 {:?}",
            report.elapsed
        );
    }
}
