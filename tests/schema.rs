//! schema 的行為契約。
//!
//! 這些測試對真實的 SQLite 執行，不使用替身。專案的問題多半出在 SQL
//! 與交易邊界上。

use std::path::PathBuf;

use code_graph::store::{SCHEMA, SCHEMA_VERSION};
use rusqlite::Connection;

/// 建立一個套用好 schema 並開啟外鍵約束的記憶體資料庫。
///
/// `foreign_keys` 預設關閉，不開啟則 `ON DELETE CASCADE` 不會生效，
/// 刪除檔案會留下孤兒符號且沒有錯誤訊息。
fn fresh_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    conn
}

/// 塞入一個最小可用的專案：一個單元、一個檔案、兩個符號。
fn seed(conn: &Connection) {
    conn.execute_batch(
        "INSERT INTO units(id, name) VALUES (1, 'root');
         INSERT INTO files(id, path, unit_id, content_hash, indexed_at)
             VALUES (1, 'src/a.rs', 1, 'hash-a', 0);
         INSERT INTO symbols(id, name, kind, file_id, start_line, end_line)
             VALUES (1, 'caller', 1, 1, 1, 5),
                    (2, 'callee', 1, 1, 7, 9);",
    )
    .unwrap();
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// schema.sql 只建表，版本列由 migrate 寫入。直接套用 schema 的資料庫
/// 版本表是空的，`Store::open` 才有辦法分辨「全新」與「已建好」。
#[test]
fn the_schema_file_does_not_record_a_version_itself() {
    let conn = fresh_db();
    assert_eq!(count(&conn, "SELECT count(*) FROM schema_versions"), 0);

    let store = code_graph::store::Store::in_memory().unwrap();
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
}

#[test]
fn deleting_a_file_cascades_to_its_symbols_and_pending_refs() {
    let conn = fresh_db();
    seed(&conn);
    conn.execute(
        "INSERT INTO unresolved_refs(from_id, ref_name, name_tail, rel, file_id, line)
         VALUES (1, 'utils.greet', 'greet', 1, 1, 3)",
        [],
    )
    .unwrap();

    conn.execute("DELETE FROM files WHERE id = 1", []).unwrap();

    assert_eq!(
        count(&conn, "SELECT count(*) FROM symbols"),
        0,
        "刪檔案沒有連帶刪掉符號，查詢會回傳已不存在的程式碼"
    );
    assert_eq!(
        count(&conn, "SELECT count(*) FROM unresolved_refs"),
        0,
        "unresolved_refs 沒有跟著 from_id 刪除"
    );
}

#[test]
fn a_symbol_cannot_reference_a_missing_file() {
    let conn = fresh_db();
    let err = conn.execute(
        "INSERT INTO symbols(id, name, kind, file_id, start_line, end_line)
         VALUES (1, 'orphan', 1, 999, 1, 2)",
        [],
    );
    assert!(err.is_err(), "外鍵沒有擋下指向不存在檔案的符號");
}

#[test]
fn identical_edges_dedup_but_different_call_sites_survive() {
    let conn = fresh_db();
    seed(&conn);

    // 同一個呼叫點索引兩次。
    for _ in 0..2 {
        conn.execute(
            "INSERT OR IGNORE INTO relations(src, dst, rel, line, file_id)
             VALUES (1, 2, 1, 3, 1)",
            [],
        )
        .unwrap();
    }
    assert_eq!(
        count(&conn, "SELECT count(*) FROM relations"),
        1,
        "重複索引產生了重複的邊，caller 數量會灌水"
    );

    // 同一組 src/dst，不同的呼叫點。
    conn.execute(
        "INSERT OR IGNORE INTO relations(src, dst, rel, line, file_id)
         VALUES (1, 2, 1, 8, 1)",
        [],
    )
    .unwrap();
    assert_eq!(
        count(&conn, "SELECT count(*) FROM relations"),
        2,
        "不同行的呼叫點被折疊掉了"
    );
}

/// 沒有位置的邊以 -1 記錄。若使用 NULL，SQLite 視每個 NULL 為相異值，
/// 主鍵擋不住重複。
#[test]
fn synthesized_edges_without_coordinates_still_dedup() {
    let conn = fresh_db();
    seed(&conn);

    for _ in 0..3 {
        conn.execute(
            "INSERT OR IGNORE INTO relations(src, dst, rel, provenance, meta)
             VALUES (1, 2, 1, 1, 'callback@src/a.rs:30')",
            [],
        )
        .unwrap();
    }

    assert_eq!(count(&conn, "SELECT count(*) FROM relations"), 1);
    let line: i64 = conn
        .query_row("SELECT line FROM relations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(line, -1, "無座標的邊應該落在 -1，而不是 NULL");
}

#[test]
fn tombstones_dedup_at_symbol_and_relation_granularity() {
    let conn = fresh_db();

    // 符號層級的刪除標記，dst 與 rel 使用預設值。
    for _ in 0..2 {
        conn.execute(
            "INSERT OR IGNORE INTO tombstones(kind, src) VALUES (1, 42)",
            [],
        )
        .unwrap();
    }
    // 邊層級的刪除標記，即使 src 相同也是另一列。
    conn.execute(
        "INSERT OR IGNORE INTO tombstones(kind, src, dst, rel) VALUES (2, 42, 43, 1)",
        [],
    )
    .unwrap();

    assert_eq!(count(&conn, "SELECT count(*) FROM tombstones"), 2);
    let dst: i64 = conn
        .query_row("SELECT dst FROM tombstones WHERE kind = 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(dst, -1);
}

#[test]
fn fts_finds_symbols_by_name_signature_and_docstring() {
    let conn = fresh_db();
    seed(&conn);
    conn.execute(
        "INSERT INTO symbols(id, name, kind, file_id, start_line, end_line, signature, docstring)
         VALUES (3, 'open_store', 1, 1, 20, 30,
                 'fn open_store(path: &Path) -> Result<Store>',
                 '開啟索引資料庫')",
        [],
    )
    .unwrap();

    for query in ["open_store", "Result", "開啟索引資料庫"] {
        let hits = count(
            &conn,
            &format!("SELECT count(*) FROM symbols_fts WHERE symbols_fts MATCH '{query}'"),
        );
        assert!(hits >= 1, "FTS 查不到 `{query}`");
    }
}

#[test]
fn fts_follows_updates_not_just_inserts() {
    let conn = fresh_db();
    seed(&conn);

    conn.execute("UPDATE symbols SET name = 'renamed_fn' WHERE id = 1", [])
        .unwrap();

    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM symbols_fts WHERE symbols_fts MATCH 'renamed_fn'"
        ),
        1,
        "update trigger 沒有把新名字寫進 FTS"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM symbols_fts WHERE symbols_fts MATCH 'caller'"
        ),
        0,
        "update trigger 沒有把舊名字從 FTS 移除"
    );
}

/// 批次索引結束時用來重建整份 FTS 索引。
#[test]
fn fts_rebuild_command_is_available() {
    let conn = fresh_db();
    seed(&conn);
    conn.execute("INSERT INTO symbols_fts(symbols_fts) VALUES('rebuild')", [])
        .unwrap();
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM symbols_fts WHERE symbols_fts MATCH 'callee'"
        ),
        1
    );
}

/// `relations` 與 `tombstones` 宣告為 WITHOUT ROWID，省去隱含的 rowid
/// 索引。
#[test]
fn hot_tables_are_without_rowid() {
    let conn = fresh_db();

    for table in ["relations", "tombstones"] {
        let err = conn
            .query_row(
                &format!("SELECT rowid FROM {table} LIMIT 1"),
                [],
                |_| Ok(()),
            )
            .unwrap_err();
        assert!(
            err.to_string().contains("no such column: rowid"),
            "{table} 似乎不是 WITHOUT ROWID，錯誤是 {err:?}"
        );
    }

    // 對照組：一般表有 rowid，空表只會回沒有資料列。
    let err = conn
        .query_row("SELECT rowid FROM symbols LIMIT 1", [], |_| Ok(()))
        .unwrap_err();
    assert!(
        matches!(err, rusqlite::Error::QueryReturnedNoRows),
        "symbols 應該是一般的 rowid 表，錯誤是 {err:?}"
    );
}

/// 路徑與 moniker 必須唯一，intern 依此判斷是新增還是重用。
#[test]
fn string_pools_reject_duplicates() {
    let conn = fresh_db();
    seed(&conn);

    let dup_path = conn.execute(
        "INSERT INTO files(id, path, unit_id, content_hash, indexed_at)
         VALUES (2, 'src/a.rs', 1, 'hash-a', 0)",
        [],
    );
    assert!(dup_path.is_err(), "同一個路徑被 intern 了兩次");

    conn.execute(
        "INSERT INTO monikers(id, moniker, handle) VALUES (1, 'src/a.rs:function:caller:1', 'a1b2c3')",
        [],
    )
    .unwrap();
    let dup_moniker = conn.execute(
        "INSERT INTO monikers(id, moniker, handle) VALUES (2, 'src/a.rs:function:caller:1', 'ffffff')",
        [],
    );
    assert!(dup_moniker.is_err(), "同一個 moniker 被 intern 了兩次");

    let dup_handle = conn.execute(
        "INSERT INTO monikers(id, moniker, handle) VALUES (3, 'src/b.rs:function:x:1', 'a1b2c3')",
        [],
    );
    assert!(dup_handle.is_err(), "handle 碰撞沒有被擋下");
}

/// 記憶體資料庫與落地檔案的行為不完全相同，兩者都要驗證。
#[test]
fn schema_applies_to_an_on_disk_database() {
    let dir = std::env::temp_dir().join(format!("codegraph-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path: PathBuf = dir.join("graph.db");
    let _ = std::fs::remove_file(&path);

    {
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        seed(&conn);
    }

    // 重新開啟後資料仍在，schema 可再次套用。
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        assert_eq!(count(&conn, "SELECT count(*) FROM symbols"), 2);
    }

    std::fs::remove_dir_all(&dir).ok();
}
