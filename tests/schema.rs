//! Schema 的行為契約（整合測試）。
//!
//! 這些測試跑真的 SQLite、真的檔案，不 mock——這個專案的 bug 大多出在
//! SQL 與交易邊界，mock 掉等於沒測（ARCHITECTURE.md §10）。

use std::path::PathBuf;

use code_graph::store::{SCHEMA, SCHEMA_VERSION};
use rusqlite::Connection;

/// 建一個套好 schema 的記憶體 DB，並開啟外鍵約束。
///
/// `foreign_keys` 預設是**關閉**的，這是 SQLite 的歷史包袱。
/// 不開的話 `ON DELETE CASCADE` 完全不會生效，刪檔案會留下孤兒符號，
/// 而且沒有任何錯誤。Stage 1 的 `Store::open` 必須把這個 pragma 寫死。
fn fresh_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    conn
}

/// 塞一個最小可用的專案：一個單元、一個檔案、兩個符號。
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

#[test]
fn schema_version_constant_is_recorded_by_migrations_not_by_the_schema_file() {
    let conn = fresh_db();
    // schema.sql 只建表，版本列是 migrate 的責任（Stage 1）。
    // 這裡確認表在、且是空的——否則 migrate 會看到來路不明的版本。
    assert_eq!(count(&conn, "SELECT count(*) FROM schema_versions"), 0);
    assert_eq!(SCHEMA_VERSION, 1);
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
        "刪檔案沒有連帶刪掉符號——孤兒符號會讓查詢回傳已經不存在的程式碼"
    );
    assert_eq!(
        count(&conn, "SELECT count(*) FROM unresolved_refs"),
        0,
        "unresolved_refs 沒有跟著 from_id 走"
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

    // 同一個 caller 在第 3 行呼叫 callee，索引跑兩次。
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
        "重複索引產生了重複的邊，會讓 caller 數量灌水"
    );

    // 同一對 src/dst，但在第 8 行又呼叫一次——這是不同的呼叫點，要保留。
    conn.execute(
        "INSERT OR IGNORE INTO relations(src, dst, rel, line, file_id)
         VALUES (1, 2, 1, 8, 1)",
        [],
    )
    .unwrap();
    assert_eq!(
        count(&conn, "SELECT count(*) FROM relations"),
        2,
        "不同行的呼叫點被錯誤地折疊掉了——呼叫點上下文會遺失"
    );
}

/// 合成邊沒有行號座標。若 `line` 可為 NULL，SQLite 會把每個 NULL 當成
/// 相異值，主鍵就擋不住重複，兩次索引會產生兩列一樣的邊。
/// 用 `NOT NULL DEFAULT -1` 就沒這個問題——這個測試釘住它。
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

    // symbol 級 tombstone：dst / rel 不適用，落在預設的 -1。
    for _ in 0..2 {
        conn.execute(
            "INSERT OR IGNORE INTO tombstones(kind, src) VALUES (1, 42)",
            [],
        )
        .unwrap();
    }
    // relation 級 tombstone：同一個 src 但完整指定，是不同的一列。
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

/// 全量索引結尾的 rebuild 必須可用（DESIGN.md §8.4）。
/// 這條指令在 external-content 表上是唯一能重建整份索引的方法。
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

/// `relations` 與 `tombstones` 宣告成 WITHOUT ROWID，省掉一整份隱含的
/// rowid 索引。這是體積承諾的一部分（DESIGN.md §8.1）。
#[test]
fn hot_tables_are_without_rowid() {
    let conn = fresh_db();

    // WITHOUT ROWID 的表沒有隱含的 rowid 欄位，SQLite 在 prepare 階段就會拒絕。
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

    // 對照組：一般表有 rowid，空表只會回「沒有資料列」。
    // 少了這一半，上面的斷言可能只是因為 SQL 打錯字而通過。
    let err = conn
        .query_row("SELECT rowid FROM symbols LIMIT 1", [], |_| Ok(()))
        .unwrap_err();
    assert!(
        matches!(err, rusqlite::Error::QueryReturnedNoRows),
        "symbols 應該是一般的 rowid 表，錯誤是 {err:?}"
    );
}

/// 路徑與 moniker 都要唯一——intern 靠這個做 upsert，
/// 重複的話同一個符號會拿到兩個 id，圖就裂成兩半。
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

/// schema 落在真的檔案上也要能跑（記憶體 DB 有些行為不同）。
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

    // 重新開啟：資料還在，schema 可以再套一次。
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        assert_eq!(count(&conn, "SELECT count(*) FROM symbols"), 2);
    }

    std::fs::remove_dir_all(&dir).ok();
}
