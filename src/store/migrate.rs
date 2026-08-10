//! Schema 版本管理。
//!
//! 規則：`schema.sql` 永遠是「最新版的完整定義」，migration 只負責把
//! 舊 DB 帶到最新。新 DB 直接套 `schema.sql`，不重播歷史 migration。

use rusqlite::Connection;

use super::{SCHEMA, SCHEMA_VERSION};
use crate::error::{CgError, Result};

/// 把連線上的 DB 帶到 `SCHEMA_VERSION`。
///
/// - 空 DB：套用 `schema.sql`，記錄版本。
/// - 版本相同：什麼都不做。
/// - 版本較舊：依序跑 migration（目前還沒有）。
/// - 版本較新：**拒絕開啟**。用舊程式去讀新 schema 會讀到缺欄位的資料，
///   而 SQLite 不會抱怨——安靜的錯誤比明確的失敗糟得多。
pub fn migrate(conn: &Connection) -> Result<()> {
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

    // found < SCHEMA_VERSION：0 代表全新的 DB。
    conn.execute_batch(SCHEMA)?;
    apply_migrations(conn, found)?;
    record_version(conn, SCHEMA_VERSION)?;
    Ok(())
}

/// 目前 DB 的 schema 版本。全新的 DB（沒有 `schema_versions` 表，
/// 或表是空的）一律回 0。
pub fn current_version(conn: &Connection) -> Result<i64> {
    let has_table: bool = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='schema_versions'",
        [],
        |r| r.get::<_, i64>(0),
    )? > 0;

    if !has_table {
        return Ok(0);
    }

    let version: Option<i64> =
        conn.query_row("SELECT max(version) FROM schema_versions", [], |r| r.get(0))?;
    Ok(version.unwrap_or(0))
}

/// 從 `from` 版本逐級升到最新。
///
/// 目前 `SCHEMA_VERSION == 1`，沒有任何舊版本存在，所以這裡是空的。
/// 之後每加一版就補一個 arm，並且**不可以**改動已發布的 arm——
/// 使用者的 DB 會照著跑。
fn apply_migrations(conn: &Connection, from: i64) -> Result<()> {
    // 目前沒有任何 migration 需要執行 SQL；保留參數是為了讓之後
    // 新增 arm 時不必改動所有呼叫點。
    let _ = conn;

    let mut v = from;
    while v < SCHEMA_VERSION {
        match v {
            // 0 → 1：`schema.sql` 已經建好全部的表，沒有額外動作。
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

    /// 用舊程式開新 DB 必須明確失敗。安靜地少讀幾個欄位，
    /// 症狀會是「查詢結果莫名其妙變少」，幾乎不可能追。
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

    /// 版本號有跳號（例如 DB 被手動改過）時，不能默默當成沒事。
    #[test]
    fn an_unknown_upgrade_path_is_reported() {
        let conn = mem();
        conn.execute_batch(SCHEMA).unwrap();
        let err = apply_migrations(&conn, -1).unwrap_err();
        assert!(matches!(err, CgError::Corrupt { .. }));
        assert!(err.to_string().contains("-1"));
    }
}
