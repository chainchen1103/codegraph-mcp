//! `sync` 子命令：增量更新索引。
//!
//! 一次性同步掃過整個專案，只重新解析內容變過的檔案；`--watch` 則常駐
//! 監看，存檔後自動更新。

use std::fmt::Write as _;
use std::path::Path;

use crate::error::Result;
use crate::project::Project;
use crate::store::Store;
use crate::sync::{self, SyncReport, watch};

/// 同步 `path` 所屬的專案。
///
/// `watching` 為真時不會回傳，直到監看被中斷為止；每同步一批就把摘要
/// 交給 `emit`。
pub fn run(path: Option<&Path>, watching: bool, mut emit: impl FnMut(&str)) -> Result<String> {
    let start = super::resolve_start(path)?;
    let project = Project::discover(&start)?;
    let mut store = Store::open(&project.db_path())?;

    let report = sync::project(&project, &mut store)?;
    if !watching {
        return Ok(render(&report));
    }

    emit(&render(&report));
    emit(&format!("監看 {}，Ctrl-C 結束\n", project.root().display()));

    watch::run(&project, &mut store, |batch| {
        emit(&render_batch(batch));
        true
    })?;

    Ok(String::new())
}

fn render(report: &SyncReport) -> String {
    let mut out = String::new();

    writeln!(out, "更新      {}", report.updated).ok();
    if report.removed > 0 {
        writeln!(out, "移除      {}", report.removed).ok();
    }
    writeln!(out, "未變更    {}", report.unchanged).ok();
    writeln!(out, "關係      {}", report.resolved).ok();
    if report.requeued > 0 {
        writeln!(out, "  重新排入  {}", report.requeued).ok();
    }
    writeln!(out, "耗時      {} ms", report.elapsed.as_millis()).ok();

    for warning in &report.warnings {
        writeln!(out, "  ⚠ {warning}").ok();
    }

    out
}

fn render_batch(batch: &watch::Batch) -> String {
    let mut out = String::new();
    writeln!(out).ok();

    for path in &batch.paths {
        writeln!(out, "{path}").ok();
    }
    write!(out, "{}", render(&batch.report)).ok();

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CgError;
    use crate::testing::{cleanup, indexed_project, tmpdir, write};

    /// 不進入監看模式時，輸出直接回傳而不經過 `emit`。
    fn once(project: &Project) -> String {
        let mut emitted = false;
        let out = run(Some(project.root()), false, |_| emitted = true).unwrap();
        assert!(!emitted, "一次性同步不該走串流輸出");
        out
    }

    #[test]
    fn a_changed_file_is_reindexed_and_an_untouched_one_is_not() {
        let p = indexed_project("cli-sync-changed", &[("src/a.rs", "fn one() {}\n")]);
        write(&p, "src/b.rs", "fn two() {}\n");

        let out = once(&p);
        assert!(out.contains("更新      1"), "{out}");
        assert!(out.contains("未變更    1"), "{out}");

        let again = once(&p);
        assert!(again.contains("更新      0"), "第二次還在重做：{again}");

        cleanup(&p);
    }

    #[test]
    fn a_deleted_file_is_reported_as_removed() {
        let p = indexed_project(
            "cli-sync-removed",
            &[("src/a.rs", "fn one() {}\n"), ("src/b.rs", "fn two() {}\n")],
        );
        std::fs::remove_file(p.root().join("src/b.rs")).unwrap();

        let out = once(&p);
        assert!(out.contains("移除      1"), "{out}");

        cleanup(&p);
    }

    #[test]
    fn syntax_errors_are_surfaced_as_warnings() {
        let p = indexed_project("cli-sync-broken", &[("src/a.rs", "fn one() {}\n")]);
        write(&p, "src/bad.rs", "fn broken( {\n");

        let out = once(&p);
        assert!(out.contains("⚠"), "{out}");
        assert!(out.contains("src/bad.rs"), "{out}");

        cleanup(&p);
    }

    /// 監看模式下每一批都要說明動了哪些檔案。
    #[test]
    fn a_batch_lists_the_files_it_touched() {
        let batch = watch::Batch {
            paths: vec!["src/a.rs".to_string()],
            report: SyncReport {
                updated: 1,
                resolved: 2,
                requeued: 1,
                ..Default::default()
            },
        };

        let out = render_batch(&batch);
        assert!(out.contains("src/a.rs"), "{out}");
        assert!(out.contains("更新      1"), "{out}");
        assert!(out.contains("重新排入  1"), "{out}");
    }

    /// 沒有補回任何邊時不印那一行，免得每一批都多一行零。
    #[test]
    fn nothing_requeued_means_nothing_printed_about_it() {
        let out = render(&SyncReport {
            updated: 1,
            ..Default::default()
        });
        assert!(!out.contains("重新排入"), "{out}");
        assert!(!out.contains("移除"), "{out}");
    }

    #[test]
    fn syncing_without_an_index_directory_is_a_recoverable_condition() {
        let dir = tmpdir("cli-sync-bare");

        let err = run(Some(&dir), false, |_| {}).unwrap_err();
        assert!(matches!(err, CgError::NotIndexed { .. }));
        assert!(err.is_recoverable());

        std::fs::remove_dir_all(&dir).ok();
    }
}
