//! schema 版本管理。
//!
//! `schema.sql` 保存最新版的完整定義，全新的資料庫直接套用；migration
//! 只負責把既有的舊資料庫帶到最新版本。

use rusqlite::Connection;

use super::{SCHEMA, SCHEMA_VERSION};
use crate::error::{CgError, Result};

/// 將資料庫升級到 [`SCHEMA_VERSION`]。
///
/// 版本相同時不做任何事。版本較新時回 [`CgError::Corrupt`]：以舊程式
/// 讀取新 schema 會取到不完整的資料，SQLite 不會回報錯誤。
///
/// 已有內容但不屬於 codegraph 的資料庫同樣會被拒絕，避免把索引用的表
/// 建到別的檔案裡。
pub fn migrate(conn: &Connection) -> Result<()> {
    if !has_version_table(conn)? && has_user_objects(conn)? {
        return Err(CgError::Corrupt {
            detail: "這個檔案已有內容，但不是 codegraph 索引。請換一個路徑，或先移除該檔案"
                .to_string(),
        });
    }

    let found = current_version(conn)?;

    if found > SCHEMA_VERSION {
        return Err(CgError::Corrupt {
            detail: format!(
                "索引的 schema 版本是 {found}，這個程式只支援到 {SCHEMA_VERSION}。請升級 codegraph，或刪除 .codegraph/ 重新索引"
            ),
        });
    }

    if found == SCHEMA_VERSION {
        return Ok(());
    }

    // 既有的資料庫先調整既有物件，schema 才有辦法套用：新版的索引
    // 可能建在 migration 才會加上的欄位上。全新的資料庫沒有這個問題，
    // schema 一次就是最新版。
    if found > 0 {
        apply_migrations(conn, found)?;
    }

    conn.execute_batch(SCHEMA)?;

    // 欄位變動後全文檢索的內容可能對不上，重建一次。
    if found > 0 {
        super::write::rebuild_fts(conn)?;
    }

    record_version(conn, SCHEMA_VERSION)?;
    Ok(())
}

/// 資料庫目前的 schema 版本。全新的資料庫回 0。
pub fn current_version(conn: &Connection) -> Result<i64> {
    if !has_version_table(conn)? {
        return Ok(0);
    }

    let version: Option<i64> =
        conn.query_row("SELECT max(version) FROM schema_versions", [], |r| r.get(0))?;
    Ok(version.unwrap_or(0))
}

/// 是否存在版本表。它的存在即代表這個檔案是 codegraph 索引。
fn has_version_table(conn: &Connection) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='schema_versions'",
        [],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// 資料庫是否已有使用者建立的物件。
///
/// 只看 `sqlite_master`，SQLite 自己的內部物件以 `sqlite_` 開頭，排除
/// 在外。
fn has_user_objects(conn: &Connection) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
        [],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// 把既有的資料庫從 `from` 逐級升到最新版本。
///
/// 只在既有資料庫上呼叫，`from` 至少為 1。每新增一版就補一個分支，
/// 已發布的分支不可修改，使用者的資料庫會照著執行。
fn apply_migrations(conn: &Connection, from: i64) -> Result<()> {
    let mut v = from;
    while v < SCHEMA_VERSION {
        match v {
            // 1 到 2：符號加上限定名。全文檢索的欄位跟著改變，舊的
            // 虛擬表與 trigger 必須卸下，由後續的 schema 重新建立。
            1 => conn.execute_batch(
                "ALTER TABLE symbols ADD COLUMN qualified TEXT NOT NULL DEFAULT '';
                 UPDATE symbols SET qualified = name WHERE qualified = '';
                 DROP TRIGGER IF EXISTS symbols_ai;
                 DROP TRIGGER IF EXISTS symbols_ad;
                 DROP TRIGGER IF EXISTS symbols_au;
                 DROP TABLE IF EXISTS symbols_fts;",
            )?,
            other => {
                return Err(CgError::Corrupt {
                    detail: format!("沒有從版本 {other} 升級的路徑"),
                });
            }
        }
        v += 1;
    }
    Ok(())
}

fn record_version(conn: &Connection, version: i64) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO schema_versions(version, applied_at, note)
         VALUES (?1, ?2, ?3)",
        rusqlite::params![version, super::now_millis(), "schema.sql"],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn a_fresh_database_reports_version_zero() {
        assert_eq!(current_version(&mem()).unwrap(), 0);
    }

    #[test]
    fn migrate_brings_a_fresh_database_to_the_current_version() {
        let conn = mem();
        migrate(&conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);

        let tables: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='symbols'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 1);
    }

    #[test]
    fn migrate_is_idempotent() {
        let conn = mem();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();

        let rows: i64 = conn
            .query_row("SELECT count(*) FROM schema_versions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1, "重複 migrate 產生了多餘的版本列");
    }

    #[test]
    fn a_newer_schema_is_refused() {
        let conn = mem();
        migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO schema_versions(version, applied_at, note) VALUES (?1, 0, 'future')",
            [SCHEMA_VERSION + 5],
        )
        .unwrap();

        let err = migrate(&conn).unwrap_err();
        assert!(matches!(err, CgError::Corrupt { .. }));
        assert!(!err.is_recoverable(), "版本不相容不是可回復的狀況");
        assert!(err.to_string().contains("升級"));
    }

    /// 第 1 版的資料庫：符號沒有限定名，全文檢索也少一個欄位。
    fn version_one_database() -> Connection {
        let conn = mem();
        conn.execute_batch(
            "CREATE TABLE schema_versions (
                 version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL, note TEXT);
             CREATE TABLE units (
                 id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
                 export_hash TEXT NOT NULL DEFAULT '');
             CREATE TABLE files (
                 id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE,
                 unit_id INTEGER NOT NULL REFERENCES units(id),
                 is_test INTEGER NOT NULL DEFAULT 0,
                 is_generated INTEGER NOT NULL DEFAULT 0,
                 content_hash TEXT NOT NULL, indexed_at INTEGER NOT NULL);
             CREATE TABLE symbols (
                 id INTEGER PRIMARY KEY, name TEXT NOT NULL, kind INTEGER NOT NULL,
                 file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                 start_line INTEGER NOT NULL, end_line INTEGER NOT NULL,
                 signature TEXT, docstring TEXT);
             CREATE VIRTUAL TABLE symbols_fts USING fts5(
                 name, signature, docstring, content='symbols', content_rowid='id');
             INSERT INTO schema_versions(version, applied_at, note) VALUES (1, 0, 'v1');
             INSERT INTO units(id, name) VALUES (1, 'root');
             INSERT INTO files(id, path, unit_id, content_hash, indexed_at)
                 VALUES (1, 'src/a.rs', 1, 'h', 0);
             INSERT INTO symbols(id, name, kind, file_id, start_line, end_line)
                 VALUES (1, 'open', 2, 1, 10, 20);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn an_existing_database_is_upgraded_in_place() {
        let conn = version_one_database();
        migrate(&conn).unwrap();

        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);

        // 既有的資料留著，新欄位以原本的名字補上。
        let (name, qualified): (String, String) = conn
            .query_row(
                "SELECT name, qualified FROM symbols WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(name, "open");
        assert_eq!(qualified, "open", "舊資料沒有回填限定名");
    }

    #[test]
    fn upgrading_rebuilds_the_full_text_index_with_the_new_columns() {
        let conn = version_one_database();
        migrate(&conn).unwrap();

        conn.execute(
            "UPDATE symbols SET qualified = 'Store::open' WHERE id = 1",
            [],
        )
        .unwrap();

        let hits: i64 = conn
            .query_row(
                "SELECT count(*) FROM symbols_fts WHERE symbols_fts MATCH 'Store'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1, "升級後的全文檢索沒有涵蓋新欄位");
    }

    #[test]
    fn upgrading_twice_is_a_no_op() {
        let conn = version_one_database();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    /// 指向別的工具的資料庫時必須拒絕，而不是把索引用的表加進去。
    #[test]
    fn a_database_that_is_not_ours_is_refused() {
        let conn = mem();
        conn.execute_batch("CREATE TABLE unrelated(id INTEGER PRIMARY KEY, payload TEXT);")
            .unwrap();

        let err = migrate(&conn).unwrap_err();
        assert!(matches!(err, CgError::Corrupt { .. }));
        assert!(!err.is_recoverable());

        let ours: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='symbols'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ours, 0, "被拒絕的資料庫仍然被寫入了索引用的表");
    }

    /// 前一次建立中途失敗時，版本表已存在但沒有版本列。這種情況要能
    /// 續建，不能誤判成別人的資料庫。
    #[test]
    fn a_half_created_index_is_completed_rather_than_refused() {
        let conn = mem();
        conn.execute_batch(SCHEMA).unwrap();
        assert_eq!(current_version(&conn).unwrap(), 0);

        migrate(&conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    /// 索引資料庫裡另外多了使用者自建的表時，不影響開啟。
    #[test]
    fn extra_tables_alongside_our_own_do_not_trigger_the_refusal() {
        let conn = mem();
        migrate(&conn).unwrap();
        conn.execute_batch("CREATE TABLE scratch(id INTEGER PRIMARY KEY);")
            .unwrap();

        migrate(&conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn an_unknown_upgrade_path_is_reported() {
        let conn = mem();
        conn.execute_batch(SCHEMA).unwrap();
        let err = apply_migrations(&conn, -1).unwrap_err();
        assert!(matches!(err, CgError::Corrupt { .. }));
        assert!(err.to_string().contains("-1"));
    }
}
