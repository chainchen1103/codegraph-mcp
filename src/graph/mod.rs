//! 圖的查詢。

use rusqlite::Connection;

use crate::error::Result;
use crate::model::{Kind, Provenance, Rel, SymbolId};

/// 邊的一端，附上這條邊出現的位置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Neighbour {
    pub id: SymbolId,
    pub qualified: String,
    pub kind: Kind,
    /// 呼叫點所在的檔案。
    pub file: String,
    /// 呼叫點所在的行，沒有位置時為 `None`。
    pub line: Option<u32>,
    pub provenance: Provenance,
}

/// 呼叫這個符號的地方。
pub fn callers(conn: &Connection, target: SymbolId, rel: Rel) -> Result<Vec<Neighbour>> {
    neighbours(
        conn,
        "SELECT s.id, s.qualified, s.kind, f.path, r.line, r.provenance
         FROM relations r
         JOIN symbols s ON s.id = r.src
         JOIN files f ON f.id = r.file_id
         WHERE r.dst = ?1 AND r.rel = ?2
         ORDER BY f.path, r.line",
        target,
        rel,
    )
}

/// 這個符號呼叫了哪些地方。
pub fn callees(conn: &Connection, source: SymbolId, rel: Rel) -> Result<Vec<Neighbour>> {
    neighbours(
        conn,
        "SELECT s.id, s.qualified, s.kind, f.path, r.line, r.provenance
         FROM relations r
         JOIN symbols s ON s.id = r.dst
         JOIN files f ON f.id = r.file_id
         WHERE r.src = ?1 AND r.rel = ?2
         ORDER BY f.path, r.line",
        source,
        rel,
    )
}

fn neighbours(conn: &Connection, sql: &str, symbol: SymbolId, rel: Rel) -> Result<Vec<Neighbour>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(rusqlite::params![symbol.0, rel as u8], |r| {
        let raw_kind: u8 = r.get(2)?;
        let raw_line: i64 = r.get(4)?;
        let raw_provenance: u8 = r.get(5)?;
        Ok(Neighbour {
            id: SymbolId(r.get(0)?),
            qualified: r.get(1)?,
            kind: Kind::from_u8(raw_kind).unwrap_or(Kind::Function),
            file: r.get(3)?,
            // -1 代表沒有位置，例如合成的邊。
            line: (raw_line >= 0).then_some(raw_line as u32),
            provenance: Provenance::from_u8(raw_provenance).unwrap_or(Provenance::Static),
        })
    })?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract;
    use crate::resolve;
    use crate::store::Store;
    use crate::store::write::{Writer, rebuild_fts};

    fn indexed(files: &[(&str, &str)]) -> Store {
        let mut store = Store::in_memory().unwrap();
        let mut writer = Writer::new();

        store
            .with_transaction(|conn| {
                Writer::reset(conn)?;
                let unit = writer.unit(conn, "root")?;
                for (path, text) in files {
                    let parse = extract::extract(path, text).unwrap();
                    writer.write_file(conn, unit, path, "hash", &parse)?;
                }
                rebuild_fts(conn)
            })
            .unwrap();
        resolve::resolve_all(&mut store).unwrap();
        store
    }

    fn id_of(store: &Store, qualified: &str) -> SymbolId {
        SymbolId(
            store
                .conn()
                .query_row(
                    "SELECT id FROM symbols WHERE qualified = ?1",
                    [qualified],
                    |r| r.get(0),
                )
                .unwrap(),
        )
    }

    #[test]
    fn callers_lists_every_call_site() {
        let store = indexed(&[(
            "src/a.rs",
            "fn target() {}\n\
             fn one() {\n    target();\n}\n\
             fn two() {\n    target();\n    target();\n}\n",
        )]);

        let found = callers(store.conn(), id_of(&store, "target"), Rel::Calls).unwrap();

        assert_eq!(found.len(), 3, "{found:#?}");
        assert_eq!(found[0].qualified, "one");
        assert_eq!(found[0].line, Some(3));
        assert_eq!(found[1].qualified, "two");
        assert_eq!(found[1].line, Some(6));
        assert_eq!(found[2].line, Some(7));
    }

    #[test]
    fn callees_lists_what_a_symbol_calls() {
        let store = indexed(&[(
            "src/a.rs",
            "fn first() {}\nfn second() {}\nfn caller() {\n    first();\n    second();\n}\n",
        )]);

        let found = callees(store.conn(), id_of(&store, "caller"), Rel::Calls).unwrap();
        let names: Vec<&str> = found.iter().map(|n| n.qualified.as_str()).collect();
        assert_eq!(names, vec!["first", "second"]);
    }

    #[test]
    fn call_sites_carry_the_file_they_appear_in() {
        let store = indexed(&[
            ("src/a.rs", "pub fn target() {}\n"),
            ("src/b.rs", "fn caller() {\n    target();\n}\n"),
        ]);

        let found = callers(store.conn(), id_of(&store, "target"), Rel::Calls).unwrap();
        assert_eq!(found[0].file, "src/b.rs");
        assert_eq!(found[0].provenance, Provenance::Static);
    }

    #[test]
    fn a_symbol_nobody_calls_has_no_callers() {
        let store = indexed(&[("src/a.rs", "fn lonely() {}\n")]);
        assert!(
            callers(store.conn(), id_of(&store, "lonely"), Rel::Calls)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn results_are_ordered_by_file_and_line() {
        let store = indexed(&[
            ("src/b.rs", "fn later() {\n    target();\n}\n"),
            (
                "src/a.rs",
                "pub fn target() {}\nfn earlier() {\n    target();\n}\n",
            ),
        ]);

        let found = callers(store.conn(), id_of(&store, "target"), Rel::Calls).unwrap();
        let files: Vec<&str> = found.iter().map(|n| n.file.as_str()).collect();
        assert_eq!(files, vec!["src/a.rs", "src/b.rs"]);
    }
}
