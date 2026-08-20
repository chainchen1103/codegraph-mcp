//! 把定義接到它的宣告上。
//!
//! 同一個東西寫在兩個地方是常態：C/C++ 的宣告在 `.h`、定義在 `.cpp`；
//! Rust trait 的方法簽名與 `impl` 裡的本體；TypeScript interface 的方法
//! 與實作它的類別。
//!
//! 兩邊都留成獨立的符號，再連一條 [`Rel::Defines`] 從定義指向宣告。合成
//! 一個會弄丟位置——問「這個方法宣告在哪」與「本體在哪」是兩個不同的
//! 問題，兩個答案都要留著。方向朝宣告，是因為改宣告會波及定義：受影響
//! 範圍看的是入邊。
//!
//! 這一層不認識任何語言，但**認得語言邊界**：判準是同一個語言裡限定名
//! 相同，而且一邊有本體、另一邊沒有。少了語言這一維，八語言的專案裡
//! 每個 `render` 都會跟其他七個連起來——限定名只在同一個語言裡才有意義。

use std::collections::HashMap;

use rusqlite::Connection;

use crate::error::Result;
use crate::model::{Kind, Provenance, Rel};

/// 一次連接的結果。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DefinitionReport {
    /// 接上宣告的定義數。
    pub linked: usize,
    /// 有多個宣告可選、因此沒有接的定義數。
    pub ambiguous: usize,
}

/// 一個候選符號。
struct Symbol {
    id: i64,
    file_id: i64,
    has_body: bool,
}

/// 把每個定義接到同名的宣告上。
///
/// 必須在所有檔案都寫進索引之後才跑：宣告與定義多半不在同一個檔案。
pub fn link(conn: &Connection) -> Result<DefinitionReport> {
    let groups = candidates(conn)?;
    let mut report = DefinitionReport::default();

    let mut stmt = conn.prepare_cached(
        "INSERT OR IGNORE INTO relations(src, dst, rel, line, file_id, provenance)
         VALUES (?1, ?2, ?3, -1, ?4, ?5)",
    )?;

    for symbols in groups.into_values() {
        let (bodies, declarations): (Vec<_>, Vec<_>) =
            symbols.into_iter().partition(|s| s.has_body);

        // 沒有另一半就沒事可做：只有定義（一般的函數）或只有宣告
        // （沒人實作的介面方法）都是正常的。
        if bodies.is_empty() || declarations.is_empty() {
            continue;
        }
        // 多個宣告無從分辨要接哪一個，不猜。
        if declarations.len() > 1 {
            report.ambiguous += bodies.len();
            continue;
        }

        let declaration = &declarations[0];
        for body in &bodies {
            stmt.execute(rusqlite::params![
                body.id,
                declaration.id,
                Rel::Defines as u8,
                body.file_id,
                Provenance::Static as u8,
            ])?;
            report.linked += 1;
        }
    }

    Ok(report)
}

/// 依限定名分組的函數與方法。
///
/// 只看函數與方法：結構與常數沒有「宣告與定義分開」這回事，把它們算進來
/// 只會讓同名的無關符號互相連邊。
fn candidates(conn: &Connection) -> Result<HashMap<(String, String), Vec<Symbol>>> {
    let mut stmt = conn.prepare(
        "SELECT f.language, s.qualified, s.id, s.file_id, s.has_body
         FROM symbols s JOIN files f ON f.id = s.file_id
         WHERE s.kind IN (?1, ?2) AND s.qualified != ''
         ORDER BY s.id",
    )?;

    let rows = stmt.query_map([Kind::Function as u8, Kind::Method as u8], |r| {
        Ok((
            (r.get::<_, String>(0)?, r.get::<_, String>(1)?),
            Symbol {
                id: r.get(2)?,
                file_id: r.get(3)?,
                has_body: r.get::<_, i64>(4)? != 0,
            },
        ))
    })?;

    let mut groups: HashMap<(String, String), Vec<Symbol>> = HashMap::new();
    for row in rows {
        let (key, symbol) = row?;
        groups.entry(key).or_default().push(symbol);
    }
    Ok(groups)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use crate::testing::indexed;

    /// 索引之後接一次，回傳 (定義, 宣告) 的限定名配對。
    fn links(store: &Store) -> Vec<(String, String)> {
        let mut stmt = store
            .conn()
            .prepare(
                "SELECT a.qualified, b.qualified FROM relations r
                 JOIN symbols a ON a.id = r.src
                 JOIN symbols b ON b.id = r.dst
                 WHERE r.rel = ?1
                 ORDER BY a.qualified",
            )
            .unwrap();
        stmt.query_map([Rel::Defines as u8], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
    }

    /// trait 的方法簽名沒有本體，`impl` 裡的有——那是同一個方法的兩面。
    #[test]
    fn an_implementation_links_to_its_signature() {
        let store = indexed(&[(
            "src/a.rs",
            "pub trait Shape {\n    fn area(&self);\n}\n\
             pub struct Square;\n",
        )]);
        let report = store.conn();
        link(report).unwrap();

        // trait 的簽名限定名是 Shape::area，impl 的是 Square::area——
        // 限定名不同，不該連。
        assert!(links(&store).is_empty(), "{:?}", links(&store));
    }

    /// 限定名相同、一邊有本體，才連得起來。
    #[test]
    fn a_body_links_to_the_declaration_with_the_same_qualified_name() {
        let store = indexed(&[
            ("src/decl.rs", "pub trait T {\n    fn run(&self);\n}\n"),
            ("src/impl.rs", "pub trait T {\n    fn run(&self) {}\n}\n"),
        ]);

        let report = link(store.conn()).unwrap();
        assert_eq!(report.linked, 1);
        assert_eq!(
            links(&store),
            [("T::run".to_string(), "T::run".to_string())]
        );
    }

    /// 只有定義沒有宣告是常態，不該產生任何邊。
    #[test]
    fn a_plain_function_links_to_nothing() {
        let store = indexed(&[("src/a.rs", "pub fn run() {}\n")]);

        let report = link(store.conn()).unwrap();
        assert_eq!(report.linked, 0);
        assert!(links(&store).is_empty());
    }

    /// 多個宣告無從分辨，不猜。
    #[test]
    fn several_declarations_are_left_unlinked() {
        let store = indexed(&[
            ("src/a.rs", "pub trait T {\n    fn run(&self);\n}\n"),
            ("src/b.rs", "pub trait T {\n    fn run(&self);\n}\n"),
            ("src/c.rs", "pub trait T {\n    fn run(&self) {}\n}\n"),
        ]);

        let report = link(store.conn()).unwrap();
        assert_eq!(report.linked, 0);
        assert_eq!(report.ambiguous, 1);
        assert!(links(&store).is_empty());
    }

    /// 限定名只在同一個語言裡有意義。
    ///
    /// 這是修過的 bug：少了語言這一維，八語言專案裡每個 `render` 都會跟
    /// 其他七個連起來。
    #[test]
    fn a_declaration_never_links_across_languages() {
        let store = indexed(&[
            (
                "src/a.rs",
                "pub trait T {
    fn run(&self);
}
",
            ),
            (
                "web/a.ts",
                "export class T {
  run(): void {}
}
",
            ),
        ]);

        let report = link(store.conn()).unwrap();
        assert_eq!(report.linked, 0, "{:?}", links(&store));
        assert!(links(&store).is_empty());
    }

    /// 重複執行不會累積重複的邊。
    #[test]
    fn linking_twice_changes_nothing() {
        let store = indexed(&[
            ("src/decl.rs", "pub trait T {\n    fn run(&self);\n}\n"),
            ("src/impl.rs", "pub trait T {\n    fn run(&self) {}\n}\n"),
        ]);

        link(store.conn()).unwrap();
        link(store.conn()).unwrap();

        assert_eq!(links(&store).len(), 1);
    }
}
