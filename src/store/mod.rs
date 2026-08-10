//! 持久層。**唯一碰 SQL 的地方**。
//!
//! 並行模型見 ARCHITECTURE.md §4.1：寫入只有一個執行緒，
//! 讀取各自開自己的連線。這個檔案目前只提供可寫連線，
//! 唯讀連線在需要並行查詢時（Stage 4 之後）再加。

pub mod migrate;

use std::path::Path;

use rusqlite::Connection;

use crate::error::Result;

/// Schema 全文，編譯期就嵌進 binary——執行期不需要外部檔案，
/// 這是「單一靜態 binary」承諾的一部分。
pub const SCHEMA: &str = include_str!("schema.sql");

/// 目前的 schema 版本。改動 `schema.sql` 就要加 migration 並升這個數字。
pub const SCHEMA_VERSION: i64 = 1;

/// 每頁 4KB。要在 DB 產生任何頁面**之前**設定，之後再設就沒有效果。
const PAGE_SIZE: i64 = 4096;

/// 一個開啟中的索引資料庫。
pub struct Store {
    conn: Connection,
}

impl Store {
    /// 開啟（必要時建立）指定路徑的索引，並升級到最新 schema。
    pub fn open(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)?;
        Self::from_connection(conn)
    }

    /// 記憶體 DB。測試用，也是「不落地的一次性查詢」的基礎。
    pub fn in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        configure(&conn)?;
        migrate::migrate(&conn)?;
        Ok(Self { conn })
    }

    /// 底層連線。抽取／解析／查詢層透過它下 SQL——
    /// 但**只有 `store` 模組底下的程式碼**可以組 SQL 字串。
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn schema_version(&self) -> Result<i64> {
        migrate::current_version(&self.conn)
    }

    /// 索引現況。`status` 子命令與 MCP 的 `_meta` 都用它。
    pub fn stats(&self) -> Result<Stats> {
        Ok(Stats {
            files: self.count("files")?,
            symbols: self.count("symbols")?,
            relations: self.count("relations")?,
            pending_refs: self.count("unresolved_refs")?,
        })
    }

    fn count(&self, table: &str) -> Result<i64> {
        // `table` 只來自這個檔案裡的字面值，不會有外部輸入。
        Ok(self
            .conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?)
    }

    pub fn metadata(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT value FROM project_metadata WHERE key = ?1")?;
        let mut rows = stmt.query([key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    pub fn set_metadata(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO project_metadata(key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            rusqlite::params![key, value, now_millis()],
        )?;
        Ok(())
    }
}

/// 索引的規模摘要。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Stats {
    pub files: i64,
    pub symbols: i64,
    pub relations: i64,
    pub pending_refs: i64,
}

impl Stats {
    /// 還沒索引過任何東西。
    pub fn is_empty(&self) -> bool {
        self.files == 0 && self.symbols == 0
    }
}

/// 連線層級的設定。**每一條都有理由，不要當成樣板複製走。**
fn configure(conn: &Connection) -> Result<()> {
    // page_size 必須在建表之前設定，否則對已有頁面的 DB 完全無效。
    conn.pragma_update(None, "page_size", PAGE_SIZE)?;

    // WAL：讀者不會被寫者擋住。這是「查詢 < 100ms」的前提之一。
    // journal_mode 這個 pragma 會回傳一列結果，所以用 query_row 而不是 execute。
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    debug_assert!(
        mode.eq_ignore_ascii_case("wal") || mode.eq_ignore_ascii_case("memory"),
        "沒有進入 WAL 模式，實際是 {mode}"
    );

    // 外鍵預設是**關閉**的。不開的話 ON DELETE CASCADE 完全不生效，
    // 刪檔案會留下孤兒符號，而且沒有任何錯誤訊息。
    conn.pragma_update(None, "foreign_keys", true)?;

    // 崩潰時最多損失最後一次 commit，換來寫入速度。索引是可重建的衍生資料，
    // 不是使用者資料——這個取捨對這個專案是划算的。
    conn.pragma_update(None, "synchronous", "NORMAL")?;

    Ok(())
}

/// Unix epoch 毫秒。
pub(crate) fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_store_is_empty_but_migrated() {
        let store = Store::in_memory().unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);

        let stats = store.stats().unwrap();
        assert_eq!(stats, Stats::default());
        assert!(stats.is_empty());
    }

    #[test]
    fn stats_count_every_table_that_status_reports() {
        let store = Store::in_memory().unwrap();
        store
            .conn()
            .execute_batch(
                "INSERT INTO units(id, name) VALUES (1, 'root');
                 INSERT INTO files(id, path, unit_id, content_hash, indexed_at)
                     VALUES (1, 'a.rs', 1, 'h', 0);
                 INSERT INTO symbols(id, name, kind, file_id, start_line, end_line)
                     VALUES (1, 'f', 1, 1, 1, 2), (2, 'g', 1, 1, 3, 4);
                 INSERT INTO relations(src, dst, rel, line) VALUES (1, 2, 1, 1);
                 INSERT INTO unresolved_refs(from_id, ref_name, rel, file_id, line)
                     VALUES (1, 'missing', 1, 1, 2);",
            )
            .unwrap();

        let stats = store.stats().unwrap();
        assert_eq!(
            stats,
            Stats {
                files: 1,
                symbols: 2,
                relations: 1,
                pending_refs: 1,
            }
        );
        assert!(!stats.is_empty());
    }

    #[test]
    fn metadata_round_trips_and_overwrites() {
        let store = Store::in_memory().unwrap();
        assert_eq!(store.metadata("base_commit").unwrap(), None);

        store.set_metadata("base_commit", "d923955").unwrap();
        assert_eq!(
            store.metadata("base_commit").unwrap().as_deref(),
            Some("d923955")
        );

        // 同一個 key 再寫一次是更新，不是新增一列。
        store.set_metadata("base_commit", "abc1234").unwrap();
        assert_eq!(
            store.metadata("base_commit").unwrap().as_deref(),
            Some("abc1234")
        );
        let rows: i64 = store
            .conn()
            .query_row("SELECT count(*) FROM project_metadata", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }

    /// 外鍵沒開的話 CASCADE 是死的，而且不會有錯誤——
    /// 這個測試守住 `configure` 裡那一行。
    #[test]
    fn foreign_keys_are_enforced_on_every_connection() {
        let store = Store::in_memory().unwrap();
        let err = store.conn().execute(
            "INSERT INTO symbols(id, name, kind, file_id, start_line, end_line)
             VALUES (1, 'orphan', 1, 999, 1, 2)",
            [],
        );
        assert!(err.is_err(), "外鍵約束沒有生效");
    }

    #[test]
    fn page_size_is_configured_before_any_table_exists() {
        let store = Store::in_memory().unwrap();
        let size: i64 = store
            .conn()
            .query_row("PRAGMA page_size", [], |r| r.get(0))
            .unwrap();
        assert_eq!(size, PAGE_SIZE);
    }

    #[test]
    fn now_millis_is_a_plausible_timestamp() {
        // 2020-01-01 之後、2100 年之前。抓到「時鐘沒設定」這種環境問題。
        let t = now_millis();
        assert!(t > 1_577_836_800_000);
        assert!(t < 4_102_444_800_000);
    }
}
