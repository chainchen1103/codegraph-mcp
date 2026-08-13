//! 型別與資料庫表示法之間的往返測試。
//!
//! `Kind`、`Rel`、`Provenance` 在 Rust 端是列舉，在資料庫中是整數。
//! 兩邊的對應一旦不一致，讀出來的圖會是錯的，而且不會產生任何錯誤。

use code_graph::store::SCHEMA;
use code_graph::{Kind, Provenance, RawRef, Rel};
use rusqlite::Connection;

fn db_with_one_file() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    conn.execute_batch(
        "INSERT INTO units(id, name) VALUES (1, 'root');
         INSERT INTO files(id, path, unit_id, content_hash, indexed_at)
             VALUES (1, 'src/a.rs', 1, 'h', 0);",
    )
    .unwrap();
    conn
}

fn insert_symbol(conn: &Connection, id: u32, name: &str, kind: Kind) {
    conn.execute(
        "INSERT INTO symbols(id, name, qualified, kind, file_id, start_line, end_line)
         VALUES (?1, ?2, ?2, ?3, 1, 1, 2)",
        rusqlite::params![id, name, kind as u8],
    )
    .unwrap();
}

/// 讀回 kind。未知的值表示資料庫由較新的 schema 寫入，不做推測。
fn load_kind(conn: &Connection, id: u32) -> Kind {
    let raw: u8 = conn
        .query_row("SELECT kind FROM symbols WHERE id = ?1", [id], |r| r.get(0))
        .unwrap();
    Kind::from_u8(raw).unwrap_or_else(|| panic!("資料庫裡有未知的 kind: {raw}"))
}

#[test]
fn every_kind_survives_a_round_trip_through_sqlite() {
    let conn = db_with_one_file();
    let kinds = [
        Kind::Function,
        Kind::Method,
        Kind::Class,
        Kind::Struct,
        Kind::Interface,
        Kind::Trait,
        Kind::Enum,
        Kind::TypeAlias,
        Kind::Const,
        Kind::Module,
    ];

    for (i, kind) in kinds.iter().enumerate() {
        let id = i as u32 + 1;
        insert_symbol(&conn, id, &format!("sym_{i}"), *kind);
        assert_eq!(load_kind(&conn, id), *kind);
    }
}

#[test]
fn optional_fields_round_trip_as_null() {
    let conn = db_with_one_file();
    insert_symbol(&conn, 1, "bare", Kind::Function);
    conn.execute(
        "INSERT INTO symbols(id, name, qualified, kind, file_id, start_line, end_line,
                             signature, docstring)
         VALUES (2, 'documented', 'documented', 1, 1, 5, 9, 'fn documented()', '說明文字')",
        [],
    )
    .unwrap();

    let bare: (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT signature, docstring FROM symbols WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(bare, (None, None));

    let documented: (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT signature, docstring FROM symbols WHERE id = 2",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(documented.0.as_deref(), Some("fn documented()"));
    assert_eq!(documented.1.as_deref(), Some("說明文字"));
}

#[test]
fn every_rel_and_provenance_survives_a_round_trip() {
    let conn = db_with_one_file();
    for i in 1..=3u32 {
        insert_symbol(&conn, i, &format!("s{i}"), Kind::Function);
    }

    let rels = [
        Rel::Calls,
        Rel::Implements,
        Rel::UsesType,
        Rel::Extends,
        Rel::References,
        Rel::Contains,
    ];

    for (i, rel) in rels.iter().enumerate() {
        // 交錯兩種來源，順便確認合成的邊一定帶著說明。
        let (provenance, meta) = if i % 2 == 0 {
            (Provenance::Static, None)
        } else {
            (Provenance::Heuristic, Some(format!("synth-{i}")))
        };

        conn.execute(
            "INSERT INTO relations(src, dst, rel, line, file_id, provenance, meta)
             VALUES (1, 2, ?1, ?2, 1, ?3, ?4)",
            rusqlite::params![*rel as u8, i as i64 + 1, provenance as u8, meta],
        )
        .unwrap();
    }

    let mut stmt = conn
        .prepare("SELECT rel, provenance, meta FROM relations ORDER BY line")
        .unwrap();
    let loaded: Vec<(Rel, Provenance, Option<String>)> = stmt
        .query_map([], |row| {
            let rel_raw: u8 = row.get(0)?;
            let prov_raw: u8 = row.get(1)?;
            Ok((
                Rel::from_u8(rel_raw).expect("未知的 rel"),
                Provenance::from_u8(prov_raw).expect("未知的 provenance"),
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    assert_eq!(loaded.len(), rels.len());
    for (i, (rel, provenance, meta)) in loaded.iter().enumerate() {
        assert_eq!(*rel, rels[i]);
        if i % 2 == 0 {
            assert_eq!(*provenance, Provenance::Static);
            assert!(meta.is_none(), "靜態邊不該帶合成器資訊");
        } else {
            assert_eq!(*provenance, Provenance::Heuristic);
            assert!(meta.is_some(), "合成的邊必須記錄來源");
        }
    }
}

/// `RawRef` 會寫進 `unresolved_refs` 等待解析，`name_tail` 是重試時的
/// 查找鍵。
#[test]
fn raw_refs_land_in_unresolved_with_a_usable_name_tail() {
    let conn = db_with_one_file();
    insert_symbol(&conn, 1, "caller", Kind::Function);

    // 抽取階段只有 moniker，寫入前必須先 intern 成識別碼。
    let raw = RawRef {
        from: "src/a.rs:function:caller:1".into(),
        name: "utils.greet".into(),
        rel: Rel::Calls,
        line: 3,
    };
    let tail = raw.name.rsplit('.').next().unwrap().to_string();

    conn.execute(
        "INSERT INTO unresolved_refs(from_id, ref_name, name_tail, rel, file_id, line, status)
         VALUES (1, ?1, ?2, ?3, 1, ?4, 1)",
        rusqlite::params![raw.name, tail, raw.rel as u8, raw.line],
    )
    .unwrap();

    // 新符號名為 greet 時，要能靠 name_tail 找回這條待解析的引用。
    let found: i64 = conn
        .query_row(
            "SELECT count(*) FROM unresolved_refs WHERE status = 1 AND name_tail = 'greet'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(found, 1);
}
