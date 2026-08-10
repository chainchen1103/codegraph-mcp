//! 持久層。**唯一碰 SQL 的地方**。
//!
//! Stage 0 只把 schema 編進 binary。開啟連線、migration、讀寫
//! 在 Stage 1 之後陸續加入（見 IMPLEMENTATION.md）。

/// Schema 全文，編譯期就嵌進 binary——執行期不需要外部檔案，
/// 這是「單一靜態 binary」承諾的一部分。
pub const SCHEMA: &str = include_str!("schema.sql");

/// 目前的 schema 版本。改動 `schema.sql` 就要加 migration 並升這個數字。
pub const SCHEMA_VERSION: i64 = 1;

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// schema.sql 一定要能在乾淨的記憶體 DB 上跑完。
    /// 這個測試也順便證明 rusqlite 的 bundled SQLite 真的有 FTS5——
    /// 沒有的話 CREATE VIRTUAL TABLE 會直接失敗。
    #[test]
    fn schema_applies_to_a_fresh_database() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();

        let tables: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
                .unwrap();
            stmt.query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };

        for expected in [
            "files",
            "monikers",
            "project_metadata",
            "relations",
            "schema_versions",
            "symbols",
            "symbols_fts",
            "tombstones",
            "units",
            "unresolved_refs",
        ] {
            assert!(
                tables.iter().any(|t| t == expected),
                "schema 少了表 {expected}，實際有：{tables:?}"
            );
        }
    }

    /// schema.sql 必須可以重複套用（`IF NOT EXISTS` 全覆蓋）。
    /// 開啟既有專案時會再跑一次。
    #[test]
    fn schema_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
    }

    /// FTS5 的 trigger 真的有把資料同步過去。
    /// 漏掉同步的症狀是搜尋靜默回空，沒有任何錯誤——所以要有測試。
    #[test]
    fn fts_triggers_keep_the_index_in_sync() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute_batch(
            "INSERT INTO units(id, name) VALUES (1, 'root');
             INSERT INTO files(id, path, unit_id, content_hash, indexed_at)
                 VALUES (1, 'src/a.rs', 1, 'h', 0);
             INSERT INTO symbols(id, name, kind, file_id, start_line, end_line)
                 VALUES (1, 'open_store', 1, 1, 10, 20);",
        )
        .unwrap();

        let hits: i64 = conn
            .query_row(
                "SELECT count(*) FROM symbols_fts WHERE symbols_fts MATCH 'open_store'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1, "insert trigger 沒有把符號同步進 FTS");

        conn.execute("DELETE FROM symbols WHERE id = 1", [])
            .unwrap();
        let hits: i64 = conn
            .query_row(
                "SELECT count(*) FROM symbols_fts WHERE symbols_fts MATCH 'open_store'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 0, "delete trigger 沒有把符號從 FTS 移除");
    }
}
