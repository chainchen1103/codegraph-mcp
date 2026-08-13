//! `index` 子命令：重新索引整個專案。

use std::fmt::Write as _;
use std::path::Path;

use crate::error::Result;
use crate::indexer;
use crate::project::Project;
use crate::store::Store;

/// 最多列出幾則警告，其餘以總數帶過。
const MAX_WARNINGS_SHOWN: usize = 10;

/// 佔比，分母為零時回 0。
fn percent(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    part as f64 / whole as f64 * 100.0
}

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
    writeln!(out).ok();

    let resolve = &report.resolve;
    writeln!(out, "關係      {}", resolve.resolved).ok();
    if resolve.guessed > 0 {
        writeln!(
            out,
            "  其中推測  {}（{:.0}%）",
            resolve.guessed,
            percent(resolve.guessed, resolve.resolved)
        )
        .ok();
    }
    writeln!(
        out,
        "待解析    {}（{:.1}%）",
        resolve.ambiguous,
        resolve.pending_ratio() * 100.0
    )
    .ok();
    writeln!(out, "外部呼叫  {}", resolve.external).ok();
    writeln!(out).ok();
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
    use crate::testing::{cleanup, tmp_project, tmpdir};

    #[test]
    fn index_reports_what_it_wrote() {
        let p = tmp_project(
            "cli-index-report",
            &[("src/a.rs", "fn one() {}\nfn two() {}\n")],
        );

        let out = run(Some(p.root())).unwrap();
        assert!(out.contains("檔案      1"), "{out}");
        assert!(out.contains("符號      2"), "{out}");
        assert!(out.contains("耗時"), "{out}");
        assert!(!out.contains("警告"), "沒有問題時不該印警告：{out}");

        cleanup(&p);
    }

    #[test]
    fn warnings_are_listed_with_a_cap() {
        let broken: Vec<(String, &str)> = (0..(MAX_WARNINGS_SHOWN + 3))
            .map(|i| (format!("src/bad{i}.rs"), "fn broken( {\n"))
            .collect();
        let files: Vec<(&str, &str)> = broken.iter().map(|(p, b)| (p.as_str(), *b)).collect();
        let p = tmp_project("cli-index-warnings", &files);

        let out = run(Some(p.root())).unwrap();
        assert!(out.contains("警告"), "{out}");
        assert!(out.contains("還有 3 則"), "超出上限的警告要收合：{out}");

        cleanup(&p);
    }

    #[test]
    fn indexing_without_an_index_directory_is_a_recoverable_condition() {
        let dir = tmpdir("cli-index-bare");

        let err = run(Some(&dir)).unwrap_err();
        assert!(matches!(err, CgError::NotIndexed { .. }));
        assert!(err.is_recoverable());

        std::fs::remove_dir_all(&dir).ok();
    }
}
