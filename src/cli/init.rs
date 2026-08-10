//! `codegraph init` —— 建立 `.codegraph/`。
//!
//! **不會自動開始索引。** 索引是使用者的決定，工具不得自作主張
//! （ARCHITECTURE.md §12）。

use std::fmt::Write as _;
use std::path::Path;

use crate::error::Result;
use crate::project::Project;
use crate::store::Store;

pub fn run(path: Option<&Path>) -> Result<String> {
    let root = super::resolve_start(path)?;
    let already = Project::is_initialized(&root);

    let project = Project::create(&root)?;
    // 開一次就會建好檔案並套用 schema。重跑時這一步是 no-op。
    let store = Store::open(&project.db_path())?;

    let mut out = String::new();
    if already {
        writeln!(out, "索引已存在：{}", project.dir().display()).ok();
    } else {
        writeln!(out, "已建立索引目錄：{}", project.dir().display()).ok();
    }
    writeln!(out, "  資料庫    {}", project.db_path().display()).ok();
    writeln!(out, "  設定      {}", project.config_path().display()).ok();
    writeln!(out, "  schema    v{}", store.schema_version()?).ok();
    writeln!(out).ok();
    writeln!(out, "下一步：codegraph index").ok();

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("codegraph-init-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn init_creates_everything_a_later_command_needs() {
        let root = tmpdir("fresh");
        let out = run(Some(&root)).unwrap();

        assert!(out.contains("已建立索引目錄"));
        assert!(out.contains("schema    v1"));

        let project = Project::discover(&root).unwrap();
        assert!(project.db_path().is_file(), "沒有建立資料庫檔案");
        assert!(project.config_path().is_file(), "沒有建立設定檔");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn init_says_so_when_it_is_a_no_op() {
        let root = tmpdir("twice");
        run(Some(&root)).unwrap();
        let out = run(Some(&root)).unwrap();

        assert!(
            out.contains("索引已存在"),
            "重跑 init 應該說明是 no-op：{out}"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// 重跑 init 不可以清空既有索引——使用者可能只是想確認狀態。
    #[test]
    fn init_never_wipes_an_existing_index() {
        let root = tmpdir("preserve");
        run(Some(&root)).unwrap();

        let project = Project::discover(&root).unwrap();
        {
            let store = Store::open(&project.db_path()).unwrap();
            store.set_metadata("base_commit", "d923955").unwrap();
        }

        run(Some(&root)).unwrap();

        let store = Store::open(&project.db_path()).unwrap();
        assert_eq!(
            store.metadata("base_commit").unwrap().as_deref(),
            Some("d923955"),
            "重跑 init 把既有索引清掉了"
        );

        std::fs::remove_dir_all(&root).ok();
    }
}
