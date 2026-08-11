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

    conn.execute_batch(SCHEMA)?;
    apply_migrations(conn, found)?;
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

/// 從 `from` 逐級升到最新版本。
///
/// 每新增一版就補一個分支。已發布的分支不可修改，使用者的資料庫會照
/// 著執行。
fn apply_migrations(conn: &Connection, from: i64) -> Result<()> {
    // 目前沒有任何一版需要額外的 SQL。
    let _ = conn;

    let mut v = from;
    while v < SCHEMA_VERSION {
        match v {
            // 0 到 1：schema.sql 已建立全部的表。
            0 => {}
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
