//! 索引資料庫的存取層。
//!
//! SQL 只出現在這個模組底下。寫入使用單一連線，讀取端各自開啟自己的
//! 連線。

pub mod intern;
pub mod migrate;
pub mod write;

use std::path::Path;

use rusqlite::Connection;

use crate::error::Result;

/// schema 全文，於編譯期嵌入，執行期不需要外部檔案。
pub const SCHEMA: &str = include_str!("schema.sql");

/// 目前的 schema 版本。修改 `schema.sql` 時必須同步遞增並提供 migration。
pub const SCHEMA_VERSION: i64 = 6;

/// 資料庫頁大小。必須在資料庫產生任何頁面之前設定，之後設定無效。
const PAGE_SIZE: i64 = 4096;

/// 一個開啟中的索引資料庫。
#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

impl Store {
    /// 開啟指定路徑的索引，必要時建立檔案並套用最新 schema。
    pub fn open(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)?;
        Self::from_connection(conn)
    }

    /// 開啟記憶體資料庫。
    pub fn in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        configure(&conn)?;
        migrate::migrate(&conn)?;
        Ok(Self { conn })
    }

    /// 底層連線。
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// 在一個交易中執行 `f`。
    ///
    /// `f` 回傳錯誤時交易會回滾，資料庫停留在交易開始前的狀態。批次
    /// 寫入必須走這個入口：中途失敗若留下一半的資料，查詢會安靜地少
    /// 回結果。
    pub fn with_transaction<T>(&mut self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let tx = self.conn.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// 資料庫目前的 schema 版本。
    pub fn schema_version(&self) -> Result<i64> {
        migrate::current_version(&self.conn)
    }

    /// 索引規模摘要。
    pub fn stats(&self) -> Result<Stats> {
        Ok(Stats {
            files: self.count("files")?,
            symbols: self.count("symbols")?,
            relations: self.count("relations")?,
            pending_refs: self.count_pending_refs()?,
        })
    }

    /// 還有機會解析出來的引用數。
    ///
    /// 增量同步會把索引裡查無此名的引用留在佇列裡，等新符號出現時重試。
    /// 那些是專案外部的呼叫，不屬於「圖還缺這一塊」，不列入計數。
    fn count_pending_refs(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM unresolved_refs WHERE status != 2",
            [],
            |r| r.get(0),
        )?)
    }

    /// 計算單一表的列數。`table` 僅接受本模組內的字面值。
    fn count(&self, table: &str) -> Result<i64> {
        Ok(self
            .conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?)
    }

    /// 讀取專案中繼資料，不存在時回 `None`。
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

    /// 寫入專案中繼資料，同名的鍵會被更新。
    pub fn set_metadata(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO project_metadata(key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            rusqlite::params![key, value, now_millis()],
        )?;
        Ok(())
    }
}

/// 索引規模摘要。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Stats {
    pub files: i64,
    pub symbols: i64,
    pub relations: i64,
    pub pending_refs: i64,
}

impl Stats {
    /// 索引中還沒有任何內容。
    pub fn is_empty(&self) -> bool {
        self.files == 0 && self.symbols == 0
    }
}

/// 設定連線層級的 pragma。
fn configure(conn: &Connection) -> Result<()> {
    // 必須早於任何建表操作，否則不生效。
    conn.pragma_update(None, "page_size", PAGE_SIZE)?;

    // WAL 讓讀取不被寫入阻擋。此 pragma 會回傳一列結果，故用 query_row。
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    debug_assert!(
        mode.eq_ignore_ascii_case("wal") || mode.eq_ignore_ascii_case("memory"),
        "沒有進入 WAL 模式，實際是 {mode}"
    );

    // 外鍵預設關閉，不開啟則 ON DELETE CASCADE 不會生效且不會報錯。
    conn.pragma_update(None, "foreign_keys", true)?;

    // 索引是可重建的衍生資料，以較低的持久性換取寫入速度。
    conn.pragma_update(None, "synchronous", "NORMAL")?;

    Ok(())
}

/// 檔案內容的雜湊，對應 `files.content_hash`。
///
/// 增量同步靠它判斷一個檔案要不要重新解析。
pub fn content_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex()[..32].to_string()
}

/// 目前時間，Unix epoch 毫秒。
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

    /// 專案外部的呼叫留在佇列裡是為了日後重試，不代表圖還缺這一塊。
    #[test]
    fn retained_external_refs_are_not_counted_as_pending() {
        let store = Store::in_memory().unwrap();
        store
            .conn()
            .execute_batch(
                "INSERT INTO units(id, name) VALUES (1, 'root');
                 INSERT INTO files(id, path, unit_id, content_hash, indexed_at)
                     VALUES (1, 'a.rs', 1, 'h', 0);
                 INSERT INTO symbols(id, name, kind, file_id, start_line, end_line)
                     VALUES (1, 'f', 1, 1, 1, 2);
                 INSERT INTO unresolved_refs(from_id, ref_name, rel, file_id, line, status)
                     VALUES (1, 'ambiguous', 1, 1, 2, 1),
                            (1, 'std_call', 1, 1, 3, 2);",
            )
            .unwrap();

        assert_eq!(store.stats().unwrap().pending_refs, 1);
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

    /// 路徑上已有別的資料庫時，開啟必須失敗而不是把索引表加進去。
    #[test]
    fn opening_a_foreign_database_is_refused() {
        let dir = std::env::temp_dir().join(format!("codegraph-foreign-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("graph.db");

        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("CREATE TABLE unrelated(id INTEGER PRIMARY KEY);")
                .unwrap();
        }

        let err = Store::open(&path).unwrap_err();
        assert!(matches!(err, crate::error::CgError::Corrupt { .. }));

        let conn = Connection::open(&path).unwrap();
        let ours: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='symbols'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ours, 0, "被拒絕的資料庫仍然被寫入了索引用的表");

        drop(conn);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn content_hashes_differ_for_different_content() {
        assert_eq!(content_hash(b"same"), content_hash(b"same"));
        assert_ne!(content_hash(b"one"), content_hash(b"two"));
        assert_eq!(content_hash(b"x").len(), 32);
    }

    #[test]
    fn now_millis_is_a_plausible_timestamp() {
        // 2020 年之後、2100 年之前，可抓到時鐘未設定的環境。
        let t = now_millis();
        assert!(t > 1_577_836_800_000);
        assert!(t < 4_102_444_800_000);
    }
}
