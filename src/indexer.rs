//! 全量索引：走訪檔案、抽取符號、寫入資料庫。

use std::time::{Duration, Instant};

use crate::error::Result;
use crate::extract;
use crate::project::{self, Project, unit, walk};
use crate::resolve::{self, ResolveReport};
use crate::store::write::{FileMeta, Writer, rebuild_fts};
use crate::store::{Store, content_hash};

/// 一次交易涵蓋的檔案數。
///
/// 交易太小會讓每個檔案都付出一次 fsync，太大則在中途失敗時損失較多
/// 已完成的工作。
const BATCH_FILES: usize = 500;

/// 一次索引的結果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndexReport {
    /// 成功寫入的檔案數。
    pub files: usize,
    /// import 對應到專案內檔案的結果。
    pub imports: resolve::imports::ImportReport,
    /// 寫入的符號數。
    pub symbols: usize,
    /// 因為 moniker 重複而未寫入的符號數。
    pub skipped_symbols: usize,
    /// 引用解析的結果。
    pub resolve: ResolveReport,
    /// 讀取失敗或語法有問題的檔案。
    pub warnings: Vec<String>,
    pub elapsed: Duration,
}

/// 重新索引整個專案。
///
/// 既有的索引內容會先清空。中途失敗時，已提交的批次會留在資料庫中，
/// 重新執行即可覆蓋。
pub fn index_project(project: &Project, store: &mut Store) -> Result<IndexReport> {
    let started = Instant::now();
    let files = walk::source_files(project);

    let mut writer = Writer::new();
    let mut report = IndexReport::default();

    store.with_transaction(|conn| {
        Writer::reset(conn)?;
        writer.unit(conn, unit::ROOT)?;
        Ok(())
    })?;

    for batch in files.chunks(BATCH_FILES) {
        let mut parsed = Vec::with_capacity(batch.len());

        for file in batch {
            let absolute = file.absolute(project);
            let bytes = match std::fs::read(&absolute) {
                Ok(b) => b,
                Err(e) => {
                    report
                        .warnings
                        .push(format!("{}：讀取失敗，{e}", file.rel_path));
                    continue;
                }
            };
            let Ok(source) = String::from_utf8(bytes) else {
                report
                    .warnings
                    .push(format!("{}：不是 UTF-8 文字檔", file.rel_path));
                continue;
            };
            let Some(parse) = extract::extract(&file.rel_path, &source) else {
                continue;
            };

            report.warnings.extend(parse.errors.iter().cloned());
            parsed.push((
                file.rel_path.clone(),
                content_hash(source.as_bytes()),
                parse,
            ));
        }

        let written = store.with_transaction(|conn| {
            let mut totals = (0usize, 0usize, 0usize);
            for (rel_path, hash, parse) in &parsed {
                // 單元由檔案所在位置決定，monorepo 裡每個 manifest 各自成
                // 一個單元。
                let unit = writer.unit(conn, &unit::of(project.root(), rel_path))?;
                let module = project::module_path(rel_path);
                let language = project::language_of(rel_path);
                let w = writer.write_file(
                    conn,
                    unit,
                    FileMeta {
                        rel_path,
                        module_path: &module,
                        language,
                        content_hash: hash,
                    },
                    parse,
                )?;
                totals.0 += 1;
                totals.1 += w.symbols;
                totals.2 += w.skipped;
            }
            Ok(totals)
        })?;

        report.files += written.0;
        report.symbols += written.1;
        report.skipped_symbols += written.2;
    }

    store.with_transaction(rebuild_fts)?;
    // import 要先接上：它指名了「這個名字來自哪個檔案」，比之後靠名字
    // 比對可靠，因此必須在解析引用之前就位。
    report.imports = store.with_transaction(resolve::imports::link)?;
    report.resolve = resolve::resolve_all(store)?;
    store.set_metadata("indexed_at", &crate::store::now_millis().to_string())?;

    report.elapsed = started.elapsed();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{cleanup, tmp_project, write};

    fn index(project: &Project) -> (IndexReport, Store) {
        let mut store = Store::open(&project.db_path()).unwrap();
        let report = index_project(project, &mut store).unwrap();
        (report, store)
    }

    #[test]
    fn indexing_writes_every_supported_file() {
        let p = tmp_project("indexer-basic", &[]);
        write(&p, "src/a.rs", "fn one() {}\nfn two() {}\n");
        write(&p, "src/b.rs", "struct S;\n");
        write(&p, "README.md", "# 不是原始碼\n");

        let (report, store) = index(&p);
        assert_eq!(report.files, 2);
        assert_eq!(report.symbols, 3);
        assert!(report.warnings.is_empty());

        let stats = store.stats().unwrap();
        assert_eq!(stats.files, 2);
        assert_eq!(stats.symbols, 3);

        drop(store);
        cleanup(&p);
    }

    #[test]
    fn reindexing_replaces_rather_than_accumulates() {
        let p = tmp_project("indexer-reindex", &[]);
        write(&p, "src/a.rs", "fn one() {}\n");

        let (first, store) = index(&p);
        drop(store);

        let (second, store) = index(&p);
        assert_eq!(first.symbols, second.symbols);
        assert_eq!(store.stats().unwrap().symbols, 1, "重複索引把資料疊加了");

        drop(store);
        cleanup(&p);
    }

    #[test]
    fn deleted_files_disappear_from_the_index() {
        let p = tmp_project("indexer-deleted", &[]);
        write(&p, "src/a.rs", "fn one() {}\n");
        write(&p, "src/b.rs", "fn two() {}\n");

        let (_, store) = index(&p);
        drop(store);

        std::fs::remove_file(p.root().join("src/b.rs")).unwrap();
        let (report, store) = index(&p);

        assert_eq!(report.files, 1);
        assert_eq!(store.stats().unwrap().symbols, 1);

        drop(store);
        cleanup(&p);
    }

    #[test]
    fn indexing_is_deterministic() {
        let p = tmp_project("indexer-deterministic", &[]);
        write(&p, "src/a.rs", "fn one() {}\n");
        write(&p, "src/b.rs", "fn two() {}\n");

        let snapshot = |store: &Store| -> Vec<(i64, String, i64)> {
            let mut stmt = store
                .conn()
                .prepare("SELECT id, name, start_line FROM symbols ORDER BY id")
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
        };

        let (_, store) = index(&p);
        let first = snapshot(&store);
        drop(store);

        let (_, store) = index(&p);
        let second = snapshot(&store);

        assert_eq!(first, second, "兩次索引配發了不同的識別碼");

        drop(store);
        cleanup(&p);
    }

    #[test]
    fn syntax_errors_are_reported_but_do_not_stop_the_run() {
        let p = tmp_project("indexer-broken", &[]);
        write(&p, "src/good.rs", "fn good() {}\n");
        write(&p, "src/bad.rs", "fn broken( {\n");

        let (report, store) = index(&p);
        assert_eq!(report.files, 2);
        assert!(
            report.warnings.iter().any(|w| w.contains("src/bad.rs")),
            "語法錯誤沒有出現在警告裡：{:?}",
            report.warnings
        );
        assert!(store.stats().unwrap().symbols >= 1);

        drop(store);
        cleanup(&p);
    }

    #[test]
    fn non_utf8_files_are_reported_and_skipped() {
        let p = tmp_project("indexer-binary", &[]);
        write(&p, "src/good.rs", "fn good() {}\n");
        std::fs::write(p.root().join("src/bad.rs"), [0xff, 0xfe, 0x00, 0x01]).unwrap();

        let (report, store) = index(&p);
        assert_eq!(report.files, 1);
        assert!(
            report.warnings.iter().any(|w| w.contains("UTF-8")),
            "{:?}",
            report.warnings
        );

        drop(store);
        cleanup(&p);
    }

    #[test]
    fn an_empty_project_produces_an_empty_index() {
        let p = tmp_project("indexer-empty", &[]);
        let (report, store) = index(&p);

        assert_eq!(report.files, 0);
        assert_eq!(report.symbols, 0);
        assert!(store.stats().unwrap().is_empty());

        drop(store);
        cleanup(&p);
    }

    #[test]
    fn the_index_records_when_it_was_built() {
        let p = tmp_project("indexer-timestamp", &[]);
        write(&p, "src/a.rs", "fn one() {}\n");

        let (_, store) = index(&p);
        let recorded = store.metadata("indexed_at").unwrap().unwrap();
        assert!(recorded.parse::<i64>().unwrap() > 1_577_836_800_000);

        drop(store);
        cleanup(&p);
    }
}
