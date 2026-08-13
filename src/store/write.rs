//! 把抽取結果寫進索引。
//!
//! 寫入端維護字串池與短碼的配發，呼叫端負責交易邊界。

use std::collections::HashSet;

use rusqlite::Connection;

use super::intern::Interner;
use super::now_millis;
use crate::error::Result;
use crate::extract::FileParse;

/// 短碼的起始長度，以十六進位字元計。
const HANDLE_LENGTHS: [usize; 6] = [6, 8, 12, 16, 24, 32];

/// 單一檔案寫入後的結果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileWritten {
    pub symbols: usize,
    /// 因為 moniker 重複而未寫入的符號數。
    pub skipped: usize,
    /// 寫入待解析佇列的引用數。
    pub refs: usize,
}

/// 批次寫入器。
///
/// 一次全量索引使用一個寫入器，字串池與短碼的唯一性都由它保證。
#[derive(Debug, Default)]
pub struct Writer {
    monikers: Interner,
    paths: Interner,
    units: Interner,
    handles: HashSet<String>,
}

impl Writer {
    pub fn new() -> Self {
        Self {
            monikers: Interner::new(),
            paths: Interner::new(),
            units: Interner::new(),
            handles: HashSet::new(),
        }
    }

    /// 清空既有的索引內容，保留 schema 與版本資訊。
    ///
    /// 刪除順序由被參考的一方在後，避免觸發外鍵限制。
    pub fn reset(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "DELETE FROM relations;
             DELETE FROM unresolved_refs;
             DELETE FROM tombstones;
             DELETE FROM symbols;
             DELETE FROM files;
             DELETE FROM units;
             DELETE FROM monikers;",
        )?;
        Ok(())
    }

    /// 取得編譯單元的識別碼，必要時建立。
    pub fn unit(&mut self, conn: &Connection, name: &str) -> Result<u32> {
        let unit = self.units.intern(name);
        if unit.is_new {
            conn.execute(
                "INSERT OR IGNORE INTO units(id, name) VALUES (?1, ?2)",
                rusqlite::params![unit.id, name],
            )?;
        }
        Ok(unit.id)
    }

    /// 寫入單一檔案與其符號。
    ///
    /// `module_path` 是檔案在模組樹中的位置，由呼叫端算出：那是專案佈局
    /// 的知識，不屬於這一層。
    pub fn write_file(
        &mut self,
        conn: &Connection,
        unit_id: u32,
        rel_path: &str,
        module_path: &str,
        content_hash: &str,
        parse: &FileParse,
    ) -> Result<FileWritten> {
        let file_id = self.paths.intern(rel_path).id;

        conn.execute(
            "INSERT OR REPLACE INTO files(id, path, unit_id, is_test, is_generated, content_hash, module_path, indexed_at)
             VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7)",
            rusqlite::params![
                file_id,
                rel_path,
                unit_id,
                looks_like_test(rel_path) as i64,
                content_hash,
                module_path,
                now_millis(),
            ],
        )?;

        let mut ids = Vec::with_capacity(parse.symbols.len());
        for symbol in &parse.symbols {
            let interned = self.monikers.intern(&symbol.moniker);
            if interned.is_new {
                let handle = self.handle_for(&symbol.moniker);
                conn.execute(
                    "INSERT INTO monikers(id, moniker, handle) VALUES (?1, ?2, ?3)",
                    rusqlite::params![interned.id, symbol.moniker, handle],
                )?;
            }
            ids.push(interned.id);
        }

        let mut written = FileWritten::default();
        let mut stmt = conn.prepare_cached(
            "INSERT OR IGNORE INTO symbols(id, name, qualified, kind, file_id, start_line, end_line, signature, docstring)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;

        for (symbol, id) in parse.symbols.iter().zip(ids) {
            let rows = stmt.execute(rusqlite::params![
                id,
                symbol.name,
                symbol.qualified,
                symbol.kind as u8,
                file_id,
                symbol.start_line,
                symbol.end_line,
                symbol.signature,
                symbol.docstring,
            ])?;
            if rows == 0 {
                written.skipped += 1;
            } else {
                written.symbols += 1;
            }
        }

        written.refs = self.write_refs(conn, file_id, parse)?;
        Ok(written)
    }

    /// 把引用寫進待解析佇列。
    ///
    /// 抽取階段只知道被呼叫者寫成什麼名字，對應到哪個符號要等整個
    /// 專案都寫完才判斷得出來。
    fn write_refs(&self, conn: &Connection, file_id: u32, parse: &FileParse) -> Result<usize> {
        let mut stmt = conn.prepare_cached(
            "INSERT INTO unresolved_refs(from_id, ref_name, rel, file_id, line, status)
             VALUES (?1, ?2, ?3, ?4, ?5, 0)",
        )?;

        let mut written = 0;
        for reference in &parse.refs {
            // 發出引用的符號一定在同一個檔案裡，剛才才 intern 過。
            let Some(from_id) = self.monikers.get(&reference.from) else {
                continue;
            };
            stmt.execute(rusqlite::params![
                from_id,
                reference.name,
                reference.rel as u8,
                file_id,
                reference.line,
            ])?;
            written += 1;
        }
        Ok(written)
    }

    /// 配發短碼。
    ///
    /// 由雜湊前綴組成，長度不足以避免碰撞時逐級加長。相同的索引內容
    /// 會得到相同的結果，因為寫入順序是固定的。
    fn handle_for(&mut self, moniker: &str) -> String {
        let hex = blake3::hash(moniker.as_bytes()).to_hex();
        for len in HANDLE_LENGTHS {
            let candidate = hex[..len].to_string();
            if self.handles.insert(candidate.clone()) {
                return candidate;
            }
        }
        let full = hex.to_string();
        self.handles.insert(full.clone());
        full
    }
}

/// 重建全文檢索索引。
///
/// 批次寫入結束後執行一次，確保 FTS 與符號表一致。
pub fn rebuild_fts(conn: &Connection) -> Result<()> {
    conn.execute("INSERT INTO symbols_fts(symbols_fts) VALUES('rebuild')", [])?;
    Ok(())
}

/// 依路徑判斷是否為測試檔案。
fn looks_like_test(rel_path: &str) -> bool {
    let lower = rel_path.to_ascii_lowercase();
    let file = lower.rsplit('/').next().unwrap_or(&lower);

    lower.starts_with("tests/")
        || lower.contains("/tests/")
        || lower.contains("/test/")
        || file.starts_with("test_")
        || file.ends_with("_test.rs")
        || file.ends_with(".test.ts")
        || file.ends_with(".spec.ts")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Kind, RawSymbol};
    use crate::store::Store;

    fn symbol(moniker: &str, name: &str, line: u32) -> RawSymbol {
        RawSymbol {
            moniker: moniker.to_string(),
            name: name.to_string(),
            qualified: name.to_string(),
            kind: Kind::Function,
            start_line: line,
            end_line: line + 1,
            signature: Some(format!("fn {name}()")),
            docstring: None,
        }
    }

    fn parse_of(symbols: Vec<RawSymbol>) -> FileParse {
        FileParse {
            symbols,
            ..Default::default()
        }
    }

    fn store_with(files: &[(&str, FileParse)]) -> Store {
        let mut store = Store::in_memory().unwrap();
        let mut writer = Writer::new();
        store
            .with_transaction(|conn| {
                Writer::reset(conn)?;
                let unit = writer.unit(conn, "root")?;
                for (path, parse) in files {
                    writer.write_file(conn, unit, path, "", "hash", parse)?;
                }
                rebuild_fts(conn)
            })
            .unwrap();
        store
    }

    #[test]
    fn symbols_and_files_land_in_the_database() {
        let store = store_with(&[(
            "src/a.rs",
            parse_of(vec![
                symbol("src/a.rs:function:one:1", "one", 1),
                symbol("src/a.rs:function:two:5", "two", 5),
            ]),
        )]);

        let stats = store.stats().unwrap();
        assert_eq!(stats.files, 1);
        assert_eq!(stats.symbols, 2);

        let (name, line): (String, i64) = store
            .conn()
            .query_row(
                "SELECT name, start_line FROM symbols ORDER BY start_line LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(name, "one");
        assert_eq!(line, 1);
    }

    #[test]
    fn every_symbol_gets_a_moniker_row_with_a_handle() {
        let store = store_with(&[(
            "src/a.rs",
            parse_of(vec![symbol("src/a.rs:function:one:1", "one", 1)]),
        )]);

        let (moniker, handle): (String, String) = store
            .conn()
            .query_row("SELECT moniker, handle FROM monikers", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(moniker, "src/a.rs:function:one:1");
        assert_eq!(handle.len(), 6);
        assert!(handle.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn symbol_ids_match_their_moniker_ids() {
        let store = store_with(&[(
            "src/a.rs",
            parse_of(vec![
                symbol("src/a.rs:function:one:1", "one", 1),
                symbol("src/a.rs:function:two:5", "two", 5),
            ]),
        )]);

        let orphans: i64 = store
            .conn()
            .query_row(
                "SELECT count(*) FROM symbols WHERE id NOT IN (SELECT id FROM monikers)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphans, 0, "符號的識別碼必須對應到 monikers 的同一列");
    }

    #[test]
    fn handles_stay_unique_even_when_prefixes_collide() {
        let mut writer = Writer::new();
        let first = writer.handle_for("src/a.rs:function:one:1");

        // 直接把短碼佔掉，模擬前綴碰撞。
        let colliding = "src/b.rs:function:two:2";
        let hex = blake3::hash(colliding.as_bytes()).to_hex();
        writer.handles.insert(hex[..6].to_string());

        let second = writer.handle_for(colliding);
        assert_ne!(first, second);
        assert_eq!(second.len(), 8, "碰撞時短碼應該加長");
    }

    #[test]
    fn a_repeated_moniker_is_skipped_rather_than_failing_the_run() {
        let mut store = Store::in_memory().unwrap();
        let mut writer = Writer::new();
        let written = store
            .with_transaction(|conn| {
                Writer::reset(conn)?;
                let unit = writer.unit(conn, "root")?;
                writer.write_file(
                    conn,
                    unit,
                    "src/a.rs",
                    "",
                    "hash",
                    &parse_of(vec![
                        symbol("src/a.rs:function:dup:1", "dup", 1),
                        symbol("src/a.rs:function:dup:1", "dup", 1),
                    ]),
                )
            })
            .unwrap();

        assert_eq!(written.symbols, 1);
        assert_eq!(written.skipped, 1);
        assert_eq!(store.stats().unwrap().symbols, 1);
    }

    #[test]
    fn full_text_search_finds_written_symbols() {
        let store = store_with(&[(
            "src/a.rs",
            parse_of(vec![symbol(
                "src/a.rs:function:open_store:1",
                "open_store",
                1,
            )]),
        )]);

        let hits: i64 = store
            .conn()
            .query_row(
                "SELECT count(*) FROM symbols_fts WHERE symbols_fts MATCH 'open_store'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);
    }

    #[test]
    fn reset_clears_previous_content() {
        let mut store = Store::in_memory().unwrap();

        let mut first = Writer::new();
        store
            .with_transaction(|conn| {
                Writer::reset(conn)?;
                let unit = first.unit(conn, "root")?;
                first.write_file(
                    conn,
                    unit,
                    "src/gone.rs",
                    "",
                    "hash",
                    &parse_of(vec![symbol("src/gone.rs:function:old:1", "old", 1)]),
                )
            })
            .unwrap();

        let mut second = Writer::new();
        store
            .with_transaction(|conn| {
                Writer::reset(conn)?;
                let unit = second.unit(conn, "root")?;
                second.write_file(
                    conn,
                    unit,
                    "src/kept.rs",
                    "",
                    "hash",
                    &parse_of(vec![symbol("src/kept.rs:function:new:1", "new", 1)]),
                )
            })
            .unwrap();

        let stats = store.stats().unwrap();
        assert_eq!(stats.files, 1);
        assert_eq!(stats.symbols, 1);

        let name: String = store
            .conn()
            .query_row("SELECT name FROM symbols", [], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "new", "舊索引沒有被清乾淨");
    }

    #[test]
    fn a_failed_transaction_leaves_nothing_behind() {
        let mut store = Store::in_memory().unwrap();
        let mut writer = Writer::new();

        let outcome: Result<()> = store.with_transaction(|conn| {
            Writer::reset(conn)?;
            let unit = writer.unit(conn, "root")?;
            writer.write_file(
                conn,
                unit,
                "src/a.rs",
                "",
                "hash",
                &parse_of(vec![symbol("src/a.rs:function:one:1", "one", 1)]),
            )?;
            Err(crate::error::CgError::Corrupt {
                detail: "中途失敗".into(),
            })
        });

        assert!(outcome.is_err());
        assert_eq!(store.stats().unwrap(), crate::store::Stats::default());
    }

    #[test]
    fn units_are_created_once_and_reused() {
        let mut store = Store::in_memory().unwrap();
        let mut writer = Writer::new();
        store
            .with_transaction(|conn| {
                let a = writer.unit(conn, "root")?;
                let b = writer.unit(conn, "root")?;
                assert_eq!(a, b);
                Ok(())
            })
            .unwrap();

        let rows: i64 = store
            .conn()
            .query_row("SELECT count(*) FROM units", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn test_files_are_flagged_by_path() {
        assert!(looks_like_test("tests/schema.rs"));
        assert!(looks_like_test("src/store/tests/helper.rs"));
        assert!(looks_like_test("src/test_utils.rs"));
        assert!(looks_like_test("src/store_test.rs"));
        assert!(looks_like_test("web/app.spec.ts"));

        assert!(!looks_like_test("src/store/mod.rs"));
        assert!(!looks_like_test("src/latest.rs"));
    }

    #[test]
    fn the_test_flag_is_stored_with_the_file() {
        let store = store_with(&[
            ("src/a.rs", parse_of(vec![])),
            ("tests/b.rs", parse_of(vec![])),
        ]);

        let flagged: String = store
            .conn()
            .query_row("SELECT path FROM files WHERE is_test = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(flagged, "tests/b.rs");
    }
}
