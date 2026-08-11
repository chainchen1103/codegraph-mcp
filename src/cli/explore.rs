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
    use crate::indexer;

    fn tmp_project(tag: &str) -> Project {
        let dir = std::env::temp_dir().join(format!(
            "codegraph-cli-explore-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        Project::create(&dir).unwrap()
    }

    fn write(project: &Project, rel: &str, body: &str) {
        let path = project.root().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn index(project: &Project) {
        let mut store = Store::open(&project.db_path()).unwrap();
        indexer::index_project(project, &mut store).unwrap();
    }

    #[test]
    fn explore_returns_source_for_a_known_name() {
        let p = tmp_project("known");
        write(&p, "src/a.rs", "pub fn target() {\n    1\n}\n");
        index(&p);

        let out = run("target", Some(p.root())).unwrap();
        assert!(out.contains("## Source"), "{out}");
        assert!(out.contains("pub fn target()"), "{out}");

        std::fs::remove_dir_all(p.root()).ok();
    }

    /// 還沒索引就查詢時，要指出下一步而不是回一堆空結果。
    #[test]
    fn exploring_an_empty_index_points_at_indexing() {
        let p = tmp_project("empty");
        write(&p, "src/a.rs", "pub fn target() {}\n");

        let out = run("target", Some(p.root())).unwrap();
        assert!(out.contains("codegraph index"), "{out}");

        std::fs::remove_dir_all(p.root()).ok();
    }

    #[test]
    fn exploring_without_an_index_directory_is_a_recoverable_condition() {
        let dir =
            std::env::temp_dir().join(format!("codegraph-cli-explore-bare-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();

        let err = run("anything", Some(&dir)).unwrap_err();
        assert!(matches!(err, CgError::NotIndexed { .. }));
        assert!(err.is_recoverable());

        std::fs::remove_dir_all(&dir).ok();
    }
}
