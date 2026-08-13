//! `callers` 與 `callees` 子命令。
//!
//! 這兩個命令給人在終端機上除錯用。查詢程式碼的主要入口是 `explore`。

use std::fmt::Write as _;
use std::path::Path;

use crate::error::Result;
use crate::explore::{query, render, select};
use crate::graph::{self, Neighbour};
use crate::model::{Provenance, Rel};
use crate::project::Project;
use crate::store::Store;

/// 查詢方向。
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Direction {
    /// 誰呼叫了這個符號。
    Callers,
    /// 這個符號呼叫了誰。
    Callees,
}

impl Direction {
    fn label(self) -> &'static str {
        match self {
            Direction::Callers => "呼叫者",
            Direction::Callees => "被呼叫",
        }
    }
}

/// 列出 `symbol` 的呼叫關係。
pub fn run(symbol: &str, direction: Direction, path: Option<&Path>) -> Result<String> {
    let start = super::resolve_start(path)?;
    let project = Project::discover(&start)?;
    let store = Store::open(&project.db_path())?;

    let selection = select::select(store.conn(), &query::parse(symbol))?;
    if selection.hits.is_empty() {
        return Ok(render::not_found(&selection));
    }

    let mut out = String::new();
    for hit in &selection.hits {
        let found = match direction {
            Direction::Callers => graph::callers(store.conn(), hit.id, Rel::Calls)?,
            Direction::Callees => graph::callees(store.conn(), hit.id, Rel::Calls)?,
        };

        writeln!(
            out,
            "{} {}  {}:{}",
            hit.kind.as_str(),
            hit.qualified,
            hit.file,
            hit.start_line
        )
        .ok();
        render_neighbours(&mut out, direction, &found);
        writeln!(out).ok();
    }

    Ok(out)
}

fn render_neighbours(out: &mut String, direction: Direction, found: &[Neighbour]) {
    if found.is_empty() {
        writeln!(out, "  沒有{}", direction.label()).ok();
        return;
    }

    for n in found {
        let position = match n.line {
            Some(line) => format!("{}:{}", n.file, line),
            None => n.file.clone(),
        };
        let mark = if n.provenance == Provenance::Heuristic {
            "  ~"
        } else {
            ""
        };
        writeln!(
            out,
            "  {} {}  {}{}",
            n.kind.as_str(),
            n.qualified,
            position,
            mark
        )
        .ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{cleanup, indexed_project as tmp_project};

    #[test]
    fn callers_lists_the_call_sites_with_positions() {
        let p = tmp_project(
            "callers",
            &[
                ("src/a.rs", "pub fn target() {}\n"),
                ("src/b.rs", "fn caller() {\n    target();\n}\n"),
            ],
        );

        let out = run("target", Direction::Callers, Some(p.root())).unwrap();
        assert!(out.contains("function target"), "{out}");
        assert!(out.contains("caller"), "{out}");
        assert!(out.contains("src/b.rs:2"), "{out}");

        cleanup(&p);
    }

    #[test]
    fn callees_lists_what_the_symbol_calls() {
        let p = tmp_project(
            "callees",
            &[(
                "src/a.rs",
                "fn helper() {}\nfn caller() {\n    helper();\n}\n",
            )],
        );

        let out = run("caller", Direction::Callees, Some(p.root())).unwrap();
        assert!(out.contains("helper"), "{out}");

        cleanup(&p);
    }

    #[test]
    fn a_symbol_with_no_edges_says_so() {
        let p = tmp_project("lonely", &[("src/a.rs", "pub fn lonely() {}\n")]);

        let out = run("lonely", Direction::Callers, Some(p.root())).unwrap();
        assert!(out.contains("沒有呼叫者"), "{out}");

        cleanup(&p);
    }

    /// 同名的符號各自列出，不必先要求使用者消歧義。
    #[test]
    fn every_symbol_sharing_the_name_is_listed() {
        let p = tmp_project(
            "overloads",
            &[
                ("src/a.rs", "pub fn run() {}\n"),
                (
                    "src/b.rs",
                    "pub struct B;\nimpl B {\n    pub fn run() {}\n}\n",
                ),
            ],
        );

        let out = run("run", Direction::Callers, Some(p.root())).unwrap();
        assert!(out.contains("function run"), "{out}");
        assert!(out.contains("method B::run"), "{out}");

        cleanup(&p);
    }

    #[test]
    fn an_unknown_symbol_gets_suggestions() {
        let p = tmp_project("unknown", &[("src/a.rs", "pub fn target() {}\n")]);

        let out = run("targt", Direction::Callers, Some(p.root())).unwrap();
        assert!(out.contains("查無結果"), "{out}");
        assert!(out.contains("target"), "{out}");

        cleanup(&p);
    }
}
