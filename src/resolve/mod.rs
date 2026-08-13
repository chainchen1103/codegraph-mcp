//! 把抽取階段記下的引用解析成圖上的邊。

pub mod names;

use rusqlite::Connection;

use crate::error::Result;
use crate::model::{Provenance, Rel, SymbolId};
use crate::store::Store;

/// 待解析狀態。
const STATUS_PENDING: i64 = 0;
/// 嘗試過但無法確定目標，保留以便日後重試。
const STATUS_FAILED: i64 = 1;

/// 一次解析的結果。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResolveReport {
    /// 解析成功並寫入的邊數。
    pub resolved: usize,
    /// 有多個候選、無法確定目標的引用數。
    pub ambiguous: usize,
    /// 索引裡沒有對應符號的引用數，多半是專案外部的函數。
    pub external: usize,
    /// 只憑名字推測出來的邊數，已包含在 `resolved` 之內。
    pub guessed: usize,
}

impl ResolveReport {
    /// 屬於專案內部、卻沒能解析的比例。
    ///
    /// 分母不含外部引用：標準函式庫的呼叫本來就解析不了，把它們算進來
    /// 只會讓這個數字失去意義。
    pub fn pending_ratio(&self) -> f64 {
        let internal = self.resolved + self.ambiguous;
        if internal == 0 {
            return 0.0;
        }
        self.ambiguous as f64 / internal as f64
    }
}

/// 解析所有待處理的引用。
///
/// 全量索引結束後執行。此時所有符號都已寫入，索引裡找不到的名字就是
/// 專案外部的東西，直接丟棄；有多個候選的則保留下來，讓使用者知道
/// 圖在這裡是不完整的。
pub fn resolve_all(store: &mut Store) -> Result<ResolveReport> {
    let pending = load_pending(store.conn())?;
    let mut report = ResolveReport::default();

    store.with_transaction(|conn| {
        for row in &pending {
            let rel = Rel::from_u8(row.rel as u8).unwrap_or(Rel::Calls);
            match names::resolve(conn, &row.ref_name, row.file_id, rel)? {
                names::Match::One(target, provenance) => {
                    write_edge(conn, row, target, provenance)?;
                    discard(conn, row.id)?;
                    report.resolved += 1;
                    if provenance == Provenance::Heuristic {
                        report.guessed += 1;
                    }
                }
                names::Match::Ambiguous => {
                    mark_failed(conn, row)?;
                    report.ambiguous += 1;
                }
                names::Match::None => {
                    discard(conn, row.id)?;
                    report.external += 1;
                }
            }
        }
        Ok(())
    })?;

    Ok(report)
}

/// 一筆待解析的引用。
#[derive(Debug)]
struct Pending {
    id: i64,
    from_id: i64,
    ref_name: String,
    rel: i64,
    file_id: i64,
    line: i64,
}

fn load_pending(conn: &Connection) -> Result<Vec<Pending>> {
    let mut stmt = conn.prepare(
        "SELECT id, from_id, ref_name, rel, file_id, line
         FROM unresolved_refs WHERE status = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map([STATUS_PENDING], |r| {
        Ok(Pending {
            id: r.get(0)?,
            from_id: r.get(1)?,
            ref_name: r.get(2)?,
            rel: r.get(3)?,
            file_id: r.get(4)?,
            line: r.get(5)?,
        })
    })?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn write_edge(
    conn: &Connection,
    row: &Pending,
    target: SymbolId,
    provenance: Provenance,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO relations(src, dst, rel, line, file_id, provenance)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            row.from_id,
            target.0,
            row.rel,
            row.line,
            row.file_id,
            provenance as u8,
        ],
    )?;
    Ok(())
}

fn discard(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM unresolved_refs WHERE id = ?1", [id])?;
    Ok(())
}

fn mark_failed(conn: &Connection, row: &Pending) -> Result<()> {
    conn.execute(
        "UPDATE unresolved_refs SET status = ?1, name_tail = ?2 WHERE id = ?3",
        rusqlite::params![STATUS_FAILED, names::tail(&row.ref_name), row.id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::indexed;

    fn edges(store: &Store) -> Vec<(String, String, i64)> {
        let mut stmt = store
            .conn()
            .prepare(
                "SELECT a.qualified, b.qualified, r.line
                 FROM relations r
                 JOIN symbols a ON a.id = r.src
                 JOIN symbols b ON b.id = r.dst
                 ORDER BY r.line",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
    }

    #[test]
    fn a_call_between_two_functions_becomes_an_edge() {
        let mut store = indexed(&[(
            "src/a.rs",
            "fn callee() {}\n\nfn caller() {\n    callee();\n}\n",
        )]);

        let report = resolve_all(&mut store).unwrap();
        assert_eq!(report.resolved, 1);

        assert_eq!(
            edges(&store),
            vec![("caller".to_string(), "callee".to_string(), 4)]
        );
    }

    #[test]
    fn calls_across_files_are_resolved() {
        let mut store = indexed(&[
            ("src/a.rs", "pub fn helper() {}\n"),
            ("src/b.rs", "fn caller() {\n    helper();\n}\n"),
        ]);

        resolve_all(&mut store).unwrap();
        assert_eq!(
            edges(&store),
            vec![("caller".to_string(), "helper".to_string(), 2)]
        );
    }

    #[test]
    fn a_qualified_call_picks_the_right_type() {
        let mut store = indexed(&[(
            "src/a.rs",
            "struct A;\nimpl A {\n    fn run() {}\n}\nstruct B;\nimpl B {\n    fn run() {}\n}\n\
             fn caller() {\n    A::run();\n}\n",
        )]);

        let report = resolve_all(&mut store).unwrap();
        assert_eq!(report.resolved, 1);
        assert_eq!(edges(&store)[0].1, "A::run");
    }

    /// 兩個同名的方法無從分辨時不猜，把引用留在待解析狀態。
    ///
    /// 接收者刻意沒有型別可查：有型別標註的話抽取階段就會把它解析掉。
    #[test]
    fn an_ambiguous_call_is_kept_instead_of_guessed() {
        let mut store = indexed(&[(
            "src/a.rs",
            "struct A;\nimpl A {\n    fn run(&self) {}\n}\nstruct B;\nimpl B {\n    fn run(&self) {}\n}\n\
             fn caller() {\n    let x = make();\n    x.run();\n}\n",
        )]);

        let report = resolve_all(&mut store).unwrap();
        assert_eq!(report.ambiguous, 1);
        assert_eq!(report.resolved, 0);
        assert!(edges(&store).is_empty(), "猜出了一條不確定的邊");

        let kept: i64 = store
            .conn()
            .query_row(
                "SELECT count(*) FROM unresolved_refs WHERE status = 1 AND name_tail = 'run'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept, 1);
    }

    /// 標準函式庫的呼叫永遠解析不了，留著只會變成永久的雜訊。
    #[test]
    fn calls_to_symbols_outside_the_project_are_discarded() {
        let mut store = indexed(&[("src/a.rs", "fn caller(v: Vec<u8>) {\n    v.len();\n}\n")]);

        let report = resolve_all(&mut store).unwrap();
        assert_eq!(report.external, 1);
        assert_eq!(store.stats().unwrap().pending_refs, 0);
    }

    #[test]
    fn the_same_call_site_produces_one_edge_even_when_resolved_twice() {
        let mut store = indexed(&[(
            "src/a.rs",
            "fn callee() {}\nfn caller() {\n    callee();\n}\n",
        )]);

        resolve_all(&mut store).unwrap();
        let before = store.stats().unwrap().relations;
        resolve_all(&mut store).unwrap();
        assert_eq!(store.stats().unwrap().relations, before);
    }

    #[test]
    fn calls_at_different_lines_are_separate_edges() {
        let mut store = indexed(&[(
            "src/a.rs",
            "fn callee() {}\nfn caller() {\n    callee();\n    callee();\n}\n",
        )]);

        resolve_all(&mut store).unwrap();
        assert_eq!(store.stats().unwrap().relations, 2);
    }

    #[test]
    fn the_pending_ratio_ignores_external_calls() {
        let report = ResolveReport {
            resolved: 8,
            ambiguous: 2,
            external: 100,
            guessed: 3,
        };
        assert!((report.pending_ratio() - 0.2).abs() < 1e-9);

        assert_eq!(ResolveReport::default().pending_ratio(), 0.0);
    }
}
