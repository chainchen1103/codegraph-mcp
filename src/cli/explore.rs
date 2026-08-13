//! `explore` 子命令：依名字或問題取回相關符號的原始碼。

use std::path::Path;

use crate::error::Result;
use crate::explore;
use crate::project::Project;
use crate::store::Store;

/// 在 `path` 所屬的專案中查詢 `input`，未指定時使用工作目錄。
pub fn run(input: &str, path: Option<&Path>) -> Result<String> {
    let start = super::resolve_start(path)?;
    let project = Project::discover(&start)?;
    let store = Store::open(&project.db_path())?;

    if store.stats()?.is_empty() {
        return Ok("索引是空的。執行 codegraph index 建立索引。\n".to_string());
    }

    Ok(explore::explore(&project, &store, input)?.text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CgError;
    use crate::testing::{cleanup, indexed_project, tmp_project, tmpdir};

    #[test]
    fn explore_returns_source_for_a_known_name() {
        let p = indexed_project(
            "cli-explore-known",
            &[("src/a.rs", "pub fn target() {\n    1\n}\n")],
        );

        let out = run("target", Some(p.root())).unwrap();
        assert!(out.contains("## Source"), "{out}");
        assert!(out.contains("pub fn target()"), "{out}");

        cleanup(&p);
    }

    /// 還沒索引就查詢時，要指出下一步而不是回一堆空結果。
    #[test]
    fn exploring_an_empty_index_points_at_indexing() {
        let p = tmp_project("cli-explore-empty", &[("src/a.rs", "pub fn target() {}\n")]);

        let out = run("target", Some(p.root())).unwrap();
        assert!(out.contains("codegraph index"), "{out}");

        cleanup(&p);
    }

    #[test]
    fn exploring_without_an_index_directory_is_a_recoverable_condition() {
        let dir = tmpdir("cli-explore-bare");

        let err = run("anything", Some(&dir)).unwrap_err();
        assert!(matches!(err, CgError::NotIndexed { .. }));
        assert!(err.is_recoverable());

        std::fs::remove_dir_all(&dir).ok();
    }
}
