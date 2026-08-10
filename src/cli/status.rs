//! `codegraph status` —— 索引現況與新鮮度。
//!
//! 這是使用者判斷「這份索引還能不能信」的唯一入口，
//! 對應 MCP 回應裡的 `_meta`（DESIGN.md §4.3）。

use std::fmt::Write as _;
use std::path::Path;

use crate::error::Result;
use crate::project::Project;
use crate::store::Store;

pub fn run(path: Option<&Path>) -> Result<String> {
    let start = super::resolve_start(path)?;
    let project = Project::discover(&start)?;
    let store = Store::open(&project.db_path())?;
    let stats = store.stats()?;

    let mut out = String::new();
    writeln!(out, "專案      {}", project.root().display()).ok();
    writeln!(out, "資料庫    {}", project.db_path().display()).ok();
    writeln!(
        out,
        "          schema v{}，{}",
        store.schema_version()?,
        human_size(db_size(&project.db_path()))
    )
    .ok();
    writeln!(out).ok();
    writeln!(out, "檔案      {}", stats.files).ok();
    writeln!(out, "符號      {}", stats.symbols).ok();
    writeln!(out, "關係      {}", stats.relations).ok();
    if stats.pending_refs > 0 {
        writeln!(out, "待解析    {}", stats.pending_refs).ok();
    }
    writeln!(out).ok();

    match store.metadata("base_commit")? {
        Some(sha) => writeln!(out, "base      {sha}").ok(),
        None => writeln!(out, "base      （尚未對應任何 commit）").ok(),
    };

    if stats.is_empty() {
        writeln!(out).ok();
        writeln!(out, "索引是空的。執行 codegraph index 建立索引。").ok();
    }

    Ok(out)
}

fn db_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn human_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    match bytes {
        0 => "0 B".to_string(),
        b if b < KB => format!("{b} B"),
        b if b < MB => format!("{:.1} KB", b as f64 / KB as f64),
        b => format!("{:.1} MB", b as f64 / MB as f64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CgError;

    /// `.git` 是 repo 邊界標記，讓「找不到索引」的測試不會往上撞到
    /// 家目錄可能存在的 `.codegraph/`（見 `project::discover`）。
    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("codegraph-status-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        dir
    }

    #[test]
    fn status_on_an_empty_index_says_it_is_empty() {
        let root = tmpdir("empty");
        crate::cli::init::run(Some(&root)).unwrap();

        let out = run(Some(&root)).unwrap();
        assert!(out.contains("符號      0"));
        assert!(out.contains("索引是空的"), "空索引要明說：{out}");
        assert!(out.contains("尚未對應任何 commit"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn status_reports_counts_and_base_commit() {
        let root = tmpdir("populated");
        crate::cli::init::run(Some(&root)).unwrap();
        let project = Project::discover(&root).unwrap();

        {
            let store = Store::open(&project.db_path()).unwrap();
            store.set_metadata("base_commit", "d923955").unwrap();
            store
                .conn()
                .execute_batch(
                    "INSERT INTO units(id, name) VALUES (1, 'root');
                     INSERT INTO files(id, path, unit_id, content_hash, indexed_at)
                         VALUES (1, 'a.rs', 1, 'h', 0);
                     INSERT INTO symbols(id, name, kind, file_id, start_line, end_line)
                         VALUES (1, 'f', 1, 1, 1, 2), (2, 'g', 1, 1, 3, 4);
                     INSERT INTO relations(src, dst, rel, line) VALUES (1, 2, 1, 1);
                     INSERT INTO unresolved_refs(from_id, ref_name, rel, file_id, line)
                         VALUES (1, 'missing', 1, 1, 2);",
                )
                .unwrap();
        }

        let out = run(Some(&root)).unwrap();
        assert!(out.contains("檔案      1"));
        assert!(out.contains("符號      2"));
        assert!(out.contains("關係      1"));
        assert!(out.contains("待解析    1"));
        assert!(out.contains("d923955"));
        assert!(!out.contains("索引是空的"));

        std::fs::remove_dir_all(&root).ok();
    }

    /// 沒索引過的目錄不是錯誤，是「還沒開始」。
    #[test]
    fn status_without_an_index_is_a_recoverable_condition() {
        let root = tmpdir("bare");
        let err = run(Some(&root)).unwrap_err();
        assert!(matches!(err, CgError::NotIndexed { .. }));
        assert!(err.is_recoverable());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn sizes_are_printed_in_the_right_unit() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn a_missing_database_file_reports_zero_rather_than_failing() {
        assert_eq!(db_size(Path::new("這個檔案不存在-codegraph")), 0);
    }
}
