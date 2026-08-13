//! 三個工具的實作。
//!
//! 每個工具都是同步函式，回傳文字或 [`CgError`]；把錯誤轉成 MCP 的回應
//! 形狀是 [`super::shape`] 的事。這樣工具本身可以直接測，不必起 runtime。
//!
//! 工具數量刻意只有三個。實測結論是 agent 只可靠地呼叫一個工具，新增
//! 能力的正確落地方式是讓 `explore` 的答案變完整，不是再開一個工具。

use std::path::{Path, PathBuf};

use super::session::{self, Session};
use crate::error::Result;
use crate::explore::{budget, node, query, render, select};
use crate::project::Project;
use crate::store::Store;

/// 主入口：依問題或名字取回相關符號的原始碼、呼叫路徑與影響範圍。
pub fn explore(project_path: Option<&Path>, input: &str, session: &mut Session) -> Result<String> {
    let (project, store) = open(project_path)?;
    if store.stats()?.is_empty() {
        return Ok("索引是空的。執行 codegraph index 建立索引。\n".to_string());
    }

    let mut selection = select::select(store.conn(), &query::parse(input))?;
    let pointers = session.dedup(project.root(), &mut selection);

    // 全部都去重掉時不能走一般排版，那會被當成查無結果。
    let mut out = if selection.hits.is_empty() && !pointers.is_empty() {
        String::new()
    } else {
        let budget = budget::for_file_count(store.stats()?.files.max(0) as usize);
        let (text, emitted) = render::reporting(project.root(), &selection, budget);
        // 記的是實際送出去的，不是選中的：被額度裁掉的還沒到對方手上。
        session.record(project.root(), &selection, &emitted);
        text
    };

    session::render_pointers(&mut out, &pointers);
    Ok(out)
}

/// 深挖單一符號：完整 body 加上呼叫關係。
///
/// 不做 session 去重。指名一個符號要它的完整 body，卻拿回一行「稍早已
/// 送出」，就是沒有回答問題。
pub fn node(project_path: Option<&Path>, name: &str) -> Result<String> {
    let (project, store) = open(project_path)?;
    node::lookup(&project, &store, name)
}

/// 索引的規模與新鮮度。
pub fn status(project_path: Option<&Path>) -> Result<String> {
    crate::cli::status::run(project_path)
}

/// 找出 `project_path` 所屬的專案並開啟索引。
fn open(project_path: Option<&Path>) -> Result<(Project, Store)> {
    let start = match project_path {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir()?,
    };
    let project = Project::discover(&start)?;
    let store = Store::open(&project.db_path())?;
    Ok((project, store))
}

/// 工具參數裡的路徑，空字串視為未指定。
pub fn path_arg(raw: &Option<String>) -> Option<PathBuf> {
    raw.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CgError;
    use crate::testing::{cleanup, indexed_project, tmp_project, tmpdir};

    #[test]
    fn explore_returns_source_for_a_known_name() {
        let p = indexed_project(
            "mcp-explore",
            &[("src/a.rs", "pub fn target() {\n    1\n}\n")],
        );
        let mut session = Session::new();

        let out = explore(Some(p.root()), "target", &mut session).unwrap();
        assert!(out.contains("pub fn target()"), "{out}");

        cleanup(&p);
    }

    /// 第二次問同一件事只拿到指標，而且指標要說明自己不是缺漏。
    #[test]
    fn a_repeated_query_collapses_to_a_pointer() {
        let p = indexed_project("mcp-dedup", &[("src/a.rs", "pub fn target() {}\n")]);
        let mut session = Session::new();

        explore(Some(p.root()), "target", &mut session).unwrap();
        let again = explore(Some(p.root()), "target", &mut session).unwrap();

        assert!(!again.contains("pub fn target()"), "{again}");
        assert!(again.contains("稍早已送出"), "{again}");
        assert!(again.contains("不是缺漏"), "{again}");
        assert!(
            !again.contains("查無結果"),
            "去重不能看起來像查無結果：{again}"
        );

        cleanup(&p);
    }

    /// 只有一部分被去重時，其餘照常送出。
    #[test]
    fn a_partly_seen_answer_keeps_the_unseen_half() {
        let p = indexed_project(
            "mcp-partial",
            &[
                ("src/a.rs", "pub fn one() {}\n"),
                ("src/b.rs", "pub fn two() {}\n"),
            ],
        );
        let mut session = Session::new();

        explore(Some(p.root()), "one", &mut session).unwrap();
        let out = explore(Some(p.root()), "one two", &mut session).unwrap();

        assert!(out.contains("pub fn two()"), "{out}");
        assert!(out.contains("src/a.rs"), "指標少了已送過的檔案：{out}");

        cleanup(&p);
    }

    #[test]
    fn exploring_an_empty_index_points_at_indexing() {
        let p = tmp_project("mcp-empty", &[("src/a.rs", "pub fn target() {}\n")]);
        let mut session = Session::new();

        let out = explore(Some(p.root()), "target", &mut session).unwrap();
        assert!(out.contains("codegraph index"), "{out}");

        cleanup(&p);
    }

    #[test]
    fn node_returns_the_whole_body() {
        let p = indexed_project(
            "mcp-node",
            &[("src/a.rs", "pub fn target() {\n    1;\n}\n")],
        );

        let out = node(Some(p.root()), "target").unwrap();
        assert!(out.contains("      2 |     1;"), "{out}");
        assert!(out.contains("沒有呼叫者"), "{out}");

        cleanup(&p);
    }

    #[test]
    fn node_reports_an_unknown_name_as_recoverable() {
        let p = indexed_project("mcp-node-unknown", &[("src/a.rs", "pub fn opened() {}\n")]);

        let err = node(Some(p.root()), "opend").unwrap_err();
        assert!(matches!(err, CgError::SymbolNotFound { .. }));
        assert!(err.is_recoverable());

        cleanup(&p);
    }

    #[test]
    fn status_reports_the_index_size() {
        let p = indexed_project("mcp-status", &[("src/a.rs", "pub fn target() {}\n")]);

        let out = status(Some(p.root())).unwrap();
        assert!(out.contains("符號      1"), "{out}");

        cleanup(&p);
    }

    #[test]
    fn every_tool_reports_a_missing_index_as_recoverable() {
        let dir = tmpdir("mcp-bare");
        let mut session = Session::new();

        let cases = [
            explore(Some(&dir), "anything", &mut session).unwrap_err(),
            node(Some(&dir), "anything").unwrap_err(),
            status(Some(&dir)).unwrap_err(),
        ];
        for err in cases {
            assert!(matches!(err, CgError::NotIndexed { .. }), "{err}");
            assert!(err.is_recoverable());
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 沒有給 projectPath 時退回工作目錄。
    #[test]
    fn an_absent_path_falls_back_to_the_working_directory() {
        assert_eq!(path_arg(&None), None);
        assert_eq!(path_arg(&Some("   ".to_string())), None);
        assert_eq!(
            path_arg(&Some(" some/where ".to_string())),
            Some(PathBuf::from("some/where"))
        );
    }
}
