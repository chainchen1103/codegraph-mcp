//! 單一符號的深挖：完整 body 加上呼叫關係。
//!
//! [`crate::explore::explore`] 為了塞進預算會裁切原始碼；這裡不裁切，
//! 呼叫端指名了一個符號就是要看它的全部。名字有歧義時所有同名定義都
//! 回傳，不要求呼叫端先自行消歧義。

use std::fmt::Write as _;
use std::path::Path;

use super::{query, select};
use crate::error::{CgError, Result};
use crate::graph::{self, Neighbour};
use crate::model::{Provenance, Rel};
use crate::project::Project;
use crate::store::Store;

/// 一個符號與它兩側的呼叫關係。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub hit: select::Hit,
    pub callers: Vec<Neighbour>,
    pub callees: Vec<Neighbour>,
}

/// 查一個名字，回傳所有同名符號的完整資料。
///
/// 查不到時回 [`CgError::SymbolNotFound`] 並附上相近的名稱；這在介面層
/// 是可回復的狀況，不是失敗。
pub fn node(store: &Store, name: &str) -> Result<Vec<Node>> {
    let selection = select::select(store.conn(), &query::parse(name))?;
    if selection.hits.is_empty() {
        return Err(CgError::SymbolNotFound {
            query: name.to_string(),
            candidates: selection.suggestions,
        });
    }

    selection
        .hits
        .into_iter()
        .map(|hit| {
            Ok(Node {
                callers: graph::callers(store.conn(), hit.id, Rel::Calls)?,
                callees: graph::callees(store.conn(), hit.id, Rel::Calls)?,
                hit,
            })
        })
        .collect()
}

/// 排版成文字。
pub fn render(root: &Path, nodes: &[Node]) -> String {
    let mut out = String::new();

    for node in nodes {
        let hit = &node.hit;
        writeln!(out).ok();
        writeln!(
            out,
            "{} {}  {}:{}-{}",
            hit.kind.as_str(),
            hit.qualified,
            hit.file,
            hit.start_line,
            hit.end_line
        )
        .ok();

        render_body(&mut out, root, node);
        render_track(&mut out, "呼叫者", &node.callers);
        render_track(&mut out, "被呼叫", &node.callees);
    }

    out
}

/// 完整 body，不因預算裁切。
///
/// 原始碼從磁碟讀，索引之後的修改要立刻反映出來。
fn render_body(out: &mut String, root: &Path, node: &Node) {
    let hit = &node.hit;
    let Ok(text) = std::fs::read_to_string(root.join(&hit.file)) else {
        writeln!(out, "  （讀不到原始碼）").ok();
        if let Some(sig) = &hit.signature {
            writeln!(out, "  {sig}").ok();
        }
        return;
    };

    let start = hit.start_line.max(1) as usize;
    let end = hit.end_line.max(hit.start_line) as usize;
    for (i, line) in text
        .lines()
        .enumerate()
        .skip(start - 1)
        .take(end - start + 1)
    {
        writeln!(out, "  {:>5} | {line}", i + 1).ok();
    }
}

fn render_track(out: &mut String, label: &str, found: &[Neighbour]) {
    writeln!(out).ok();
    if found.is_empty() {
        writeln!(out, "  沒有{label}").ok();
        return;
    }

    writeln!(out, "  {label}").ok();
    for n in found {
        let position = match n.line {
            Some(line) => format!("{}:{}", n.file, line),
            None => n.file.clone(),
        };
        // 合成的邊要標出來，呼叫端才判斷得了這一跳可不可信。
        let mark = if n.provenance == Provenance::Heuristic {
            "  [heuristic]"
        } else {
            ""
        };
        writeln!(
            out,
            "    {} {}  {}{}",
            n.kind.as_str(),
            n.qualified,
            position,
            mark
        )
        .ok();
    }
}

/// 查一個名字並直接排版。
pub fn lookup(project: &Project, store: &Store, name: &str) -> Result<String> {
    Ok(render(project.root(), &node(store, name)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{cleanup, indexed_project};

    fn fixture(tag: &str, files: &[(&str, &str)]) -> (Project, Store) {
        let project = indexed_project(&format!("node-{tag}"), files);
        let store = Store::open(&project.db_path()).unwrap();
        (project, store)
    }

    #[test]
    fn a_node_carries_the_whole_body_without_clipping() {
        let (p, store) = fixture(
            "body",
            &[("src/a.rs", "pub fn target() {\n    1;\n    2;\n}\n")],
        );

        let out = lookup(&p, &store, "target").unwrap();
        assert!(out.contains("      1 | pub fn target() {"), "{out}");
        assert!(out.contains("      2 |     1;"), "{out}");
        assert!(out.contains("      4 | }"), "{out}");
        assert!(!out.contains("未列出"), "node 不該裁切：{out}");

        drop(store);
        cleanup(&p);
    }

    #[test]
    fn both_tracks_are_listed() {
        let (p, store) = fixture(
            "tracks",
            &[
                ("src/a.rs", "pub fn sink() {}\n"),
                (
                    "src/b.rs",
                    "pub fn middle() {\n    sink();\n}\n\
                     pub fn entry() {\n    middle();\n}\n",
                ),
            ],
        );

        let out = lookup(&p, &store, "middle").unwrap();
        assert!(out.contains("呼叫者"), "{out}");
        assert!(out.contains("entry"), "{out}");
        assert!(out.contains("被呼叫"), "{out}");
        assert!(out.contains("sink"), "{out}");

        drop(store);
        cleanup(&p);
    }

    #[test]
    fn a_symbol_with_no_edges_says_so_on_both_sides() {
        let (p, store) = fixture("lonely", &[("src/a.rs", "pub fn lonely() {}\n")]);

        let out = lookup(&p, &store, "lonely").unwrap();
        assert!(out.contains("沒有呼叫者"), "{out}");
        assert!(out.contains("沒有被呼叫"), "{out}");

        drop(store);
        cleanup(&p);
    }

    /// 名字有歧義時所有多載都回傳，不要求呼叫端先消歧義。
    #[test]
    fn every_overload_comes_back_at_once() {
        let (p, store) = fixture(
            "overloads",
            &[
                ("src/a.rs", "pub fn run() {}\n"),
                (
                    "src/b.rs",
                    "pub struct B;\nimpl B {\n    pub fn run() {}\n}\n",
                ),
            ],
        );

        let nodes = node(&store, "run").unwrap();
        assert_eq!(nodes.len(), 2);

        let out = lookup(&p, &store, "run").unwrap();
        assert!(out.contains("function run"), "{out}");
        assert!(out.contains("method B::run"), "{out}");

        drop(store);
        cleanup(&p);
    }

    #[test]
    fn an_unknown_name_is_a_recoverable_not_found() {
        let (p, store) = fixture("unknown", &[("src/a.rs", "pub fn opened() {}\n")]);

        let err = node(&store, "opend").unwrap_err();
        assert!(err.is_recoverable());
        match err {
            CgError::SymbolNotFound { query, candidates } => {
                assert_eq!(query, "opend");
                assert!(
                    candidates.iter().any(|c| c.contains("opened")),
                    "{candidates:?}"
                );
            }
            other => panic!("{other}"),
        }

        drop(store);
        cleanup(&p);
    }

    /// 檔案被刪掉之後仍要回答結構，只是沒有 body。
    #[test]
    fn a_missing_file_degrades_to_the_signature() {
        let (p, store) = fixture("gone", &[("src/a.rs", "pub fn target() {}\n")]);
        std::fs::remove_file(p.root().join("src/a.rs")).unwrap();

        let out = lookup(&p, &store, "target").unwrap();
        assert!(out.contains("讀不到原始碼"), "{out}");

        drop(store);
        cleanup(&p);
    }
}
