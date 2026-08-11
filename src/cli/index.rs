//! `index` 子命令：重新索引整個專案。

use std::fmt::Write as _;
use std::path::Path;

use crate::error::Result;
use crate::indexer;
use crate::project::Project;
use crate::store::Store;

/// 最多列出幾則警告，其餘以總數帶過。
const MAX_WARNINGS_SHOWN: usize = 10;

/// 索引 `path` 所屬的專案，未指定時使用工作目錄。
pub fn run(path: Option<&Path>) -> Result<String> {
    let start = super::resolve_start(path)?;
    let project = Project::discover(&start)?;
    let mut store = Store::open(&project.db_path())?;

    let report = indexer::index_project(&project, &mut store)?;

    let mut out = String::new();
    writeln!(out, "已索引 {}", project.root().display()).ok();
    writeln!(out).ok();
    writeln!(out, "檔案      {}", report.files).ok();
    writeln!(out, "符號      {}", report.symbols).ok();
    if report.skipped_symbols > 0 {
        writeln!(out, "略過      {}", report.skipped_symbols).ok();
    }
    writeln!(out, "耗時      {} ms", report.elapsed.as_millis()).ok();

    if !report.warnings.is_empty() {
        writeln!(out).ok();
        writeln!(out, "警告 {} 則", report.warnings.len()).ok();
        for w in report.warnings.iter().take(MAX_WARNINGS_SHOWN) {
            writeln!(out, "  {w}").ok();
        }
        if report.warnings.len() > MAX_WARNINGS_SHOWN {
            writeln!(
                out,
                "  還有 {} 則",
                report.warnings.len() - MAX_WARNINGS_SHOWN
            )
            .ok();
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CgError;

    fn tmp_project(tag: &str) -> Project {
        let dir =
            std::env::temp_dir().join(format!("codegraph-cli-index-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        Project::create(&dir).unwrap()
    }

    fn write(project: &Project, rel: &str, body: &str) {
        let path = project.root().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn index_reports_what_it_wrote() {
        let p = tmp_project("report");
        write(&p, "src/a.rs", "fn one() {}\nfn two() {}\n");

        let out = run(Some(p.root())).unwrap();
        assert!(out.contains("檔案      1"), "{out}");
        assert!(out.contains("符號      2"), "{out}");
        assert!(out.contains("耗時"), "{out}");
        assert!(!out.contains("警告"), "沒有問題時不該印警告：{out}");

        std::fs::remove_dir_all(p.root()).ok();
    }

    #[test]
    fn warnings_are_listed_with_a_cap() {
        let p = tmp_project("warnings");
        for i in 0..(MAX_WARNINGS_SHOWN + 3) {
            write(&p, &format!("src/bad{i}.rs"), "fn broken( {\n");
        }

        let out = run(Some(p.root())).unwrap();
        assert!(out.contains("警告"), "{out}");
        assert!(out.contains("還有 3 則"), "超出上限的警告要收合：{out}");

        std::fs::remove_dir_all(p.root()).ok();
    }

    #[test]
    fn indexing_without_an_index_directory_is_a_recoverable_condition() {
        let dir = std::env::temp_dir().join(format!("codegraph-cli-bare-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();

        let err = run(Some(&dir)).unwrap_err();
        assert!(matches!(err, CgError::NotIndexed { .. }));
        assert!(err.is_recoverable());

        std::fs::remove_dir_all(&dir).ok();
    }
}
