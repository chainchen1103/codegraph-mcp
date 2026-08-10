//! 型別與 DB 表示法之間的往返契約（整合測試）。
//!
//! `Kind` / `Rel` / `Provenance` 在 Rust 端是 enum，在 DB 裡是 integer。
//! 兩邊的對應一旦漂移，讀出來的圖就是錯的——而且不會有任何錯誤訊息，
//! 只會安靜地把 function 當成 class。這組測試把對應釘死。

use code_graph::store::SCHEMA;
use code_graph::{FileId, Kind, Provenance, RawRef, Rel, Relation, Symbol, SymbolId};
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

fn insert_symbol(conn: &Connection, s: &Symbol) {
    conn.execute(
        "INSERT INTO symbols(id, name, kind, file_id, start_line, end_line, signature, docstring)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            s.id.0,
            s.name,
            s.kind as u8,
            s.file.0,
            s.start_line,
            s.end_line,
            s.signature,
            s.docstring,
        ],
    )
    .unwrap();
}

fn load_symbol(conn: &Connection, id: SymbolId) -> Symbol {
    conn.query_row(
        "SELECT id, name, kind, file_id, start_line, end_line, signature, docstring
         FROM symbols WHERE id = ?1",
        [id.0],
        |r| {
            let raw_kind: u8 = r.get(2)?;
            Ok(Symbol {
                id: SymbolId(r.get(0)?),
                name: r.get(1)?,
                // 未知的 kind 不猜——代表這份 DB 是更新版 schema 寫的。
                kind: Kind::from_u8(raw_kind)
                    .unwrap_or_else(|| panic!("DB 裡有未知的 kind: {raw_kind}")),
                file: FileId(r.get(3)?),
                start_line: r.get(4)?,
                end_line: r.get(5)?,
                signature: r.get(6)?,
                docstring: r.get(7)?,
            })
        },
    )
    .unwrap()
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
        Kind::File,
    ];

    for (i, kind) in kinds.iter().enumerate() {
        let id = SymbolId(i as u32 + 1);
        insert_symbol(
            &conn,
            &Symbol {
                id,
                name: format!("sym_{i}"),
                kind: *kind,
                file: FileId(1),
                start_line: 1,
                end_line: 2,
                signature: None,
                docstring: None,
            },
        );
        assert_eq!(load_symbol(&conn, id).kind, *kind);
    }
}

#[test]
fn optional_fields_round_trip_as_null() {
    let conn = db_with_one_file();
    insert_symbol(
        &conn,
        &Symbol {
            id: SymbolId(1),
            name: "bare".into(),
            kind: Kind::Function,
            file: FileId(1),
            start_line: 1,
            end_line: 2,
            signature: None,
            docstring: None,
        },
    );
    insert_symbol(
        &conn,
        &Symbol {
            id: SymbolId(2),
            name: "documented".into(),
            kind: Kind::Function,
            file: FileId(1),
            start_line: 5,
            end_line: 9,
            signature: Some("fn documented()".into()),
            docstring: Some("說明文字".into()),
        },
    );

    let bare = load_symbol(&conn, SymbolId(1));
    assert!(bare.signature.is_none() && bare.docstring.is_none());

    let doc = load_symbol(&conn, SymbolId(2));
    assert_eq!(doc.signature.as_deref(), Some("fn documented()"));
    assert_eq!(doc.docstring.as_deref(), Some("說明文字"));
}

#[test]
fn every_rel_and_provenance_survives_a_round_trip() {
    let conn = db_with_one_file();
    for i in 1..=3u32 {
        insert_symbol(
            &conn,
            &Symbol {
                id: SymbolId(i),
                name: format!("s{i}"),
                kind: Kind::Function,
                file: FileId(1),
                start_line: i,
                end_line: i + 1,
                signature: None,
                docstring: None,
            },
        );
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
        let r = Relation {
            src: SymbolId(1),
            dst: SymbolId(2),
            rel: *rel,
            file: Some(FileId(1)),
            line: Some(i as u32 + 1),
            provenance: if i % 2 == 0 {
                Provenance::Static
            } else {
                Provenance::Heuristic
            },
            meta: if i % 2 == 0 {
                None
            } else {
                Some(format!("synth-{i}"))
            },
        };
        conn.execute(
            "INSERT INTO relations(src, dst, rel, line, file_id, provenance, meta)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                r.src.0,
                r.dst.0,
                r.rel as u8,
                r.line.map(|l| l as i64).unwrap_or(-1),
                r.file.map(|f| f.0),
                r.provenance as u8,
                r.meta,
            ],
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
    for (i, (rel, prov, meta)) in loaded.iter().enumerate() {
        assert_eq!(*rel, rels[i]);
        if i % 2 == 0 {
            assert_eq!(*prov, Provenance::Static);
            assert!(meta.is_none(), "靜態邊不該帶合成器資訊");
        } else {
            assert_eq!(*prov, Provenance::Heuristic);
            assert!(meta.is_some(), "合成邊一定要記錄來源（DESIGN.md §7.3）");
        }
    }
}

/// `RawRef` 是抽取層的產物，會被寫進 `unresolved_refs` 等待解析。
/// `name_tail` 是重試查找的鍵（DESIGN.md §4.2）。
#[test]
fn raw_refs_land_in_unresolved_with_a_usable_name_tail() {
    let conn = db_with_one_file();
    insert_symbol(
        &conn,
        &Symbol {
            id: SymbolId(1),
            name: "caller".into(),
            kind: Kind::Function,
            file: FileId(1),
            start_line: 1,
            end_line: 5,
            signature: None,
            docstring: None,
        },
    );

    let raw = RawRef {
        from: SymbolId(1),
        name: "utils.greet".into(),
        rel: Rel::Calls,
        line: 3,
    };
    let tail = raw.name.rsplit('.').next().unwrap().to_string();

    conn.execute(
        "INSERT INTO unresolved_refs(from_id, ref_name, name_tail, rel, file_id, line, status)
         VALUES (?1, ?2, ?3, ?4, 1, ?5, 1)",
        rusqlite::params![raw.from.0, raw.name, tail, raw.rel as u8, raw.line],
    )
    .unwrap();

    // 重試查找：新符號叫 greet，要能靠 name_tail 找回這條待解析的引用。
    let found: i64 = conn
        .query_row(
            "SELECT count(*) FROM unresolved_refs WHERE status = 1 AND name_tail = 'greet'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(found, 1);
}
