//! 專案的定位與磁碟佈局。
//!
//! 每個專案的索引放在根目錄底下的 `.codegraph/`，內含資料庫與設定檔。

use std::path::{Path, PathBuf};

use crate::error::{CgError, Result};

/// 索引目錄的名稱。
pub const DIR_NAME: &str = ".codegraph";

/// 索引資料庫的檔名。
pub const DB_NAME: &str = "graph.db";

/// 專案設定檔的檔名。
pub const CONFIG_NAME: &str = "config.toml";

const CONFIG_TEMPLATE: &str = "\
# CodeGraph 專案設定
#
# 目前僅為範本，尚未有選項被讀取。

[index]
extra_ignore = []
";

/// 一個已建立索引目錄的專案。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    root: PathBuf,
}

impl Project {
    /// 從 `start` 逐層往上尋找索引目錄，遇到 repo 邊界即停止。
    ///
    /// 含有 `.git` 的目錄視為邊界，該層本身仍會檢查。索引隸屬於單一
    /// repo，若不設邊界，未建立索引的專案會沿用上層目錄的索引，並以
    /// 另一個專案的資料回答查詢。
    ///
    /// 找不到索引時回 [`CgError::NotIndexed`]，屬於可回復的狀況。
    pub fn discover(start: &Path) -> Result<Self> {
        let start = normalize(start)?;
        for dir in start.ancestors() {
            if dir.join(DIR_NAME).is_dir() {
                return Ok(Self {
                    root: dir.to_path_buf(),
                });
            }
            // worktree 的 .git 是檔案而非目錄。
            if dir.join(".git").exists() {
                break;
            }
        }
        Err(CgError::NotIndexed { path: start })
    }

    /// 在 `root` 建立索引目錄與設定檔。
    ///
    /// 可重複呼叫。已存在的目錄與設定檔都不會被覆寫。
    pub fn create(root: &Path) -> Result<Self> {
        let root = normalize(root)?;
        let dir = root.join(DIR_NAME);
        std::fs::create_dir_all(&dir)?;

        let config = dir.join(CONFIG_NAME);
        if !config.exists() {
            std::fs::write(&config, CONFIG_TEMPLATE)?;
        }

        Ok(Self { root })
    }

    /// `root` 底下是否已有索引目錄。不會往上層尋找。
    pub fn is_initialized(root: &Path) -> bool {
        root.join(DIR_NAME).is_dir()
    }

    /// 專案根目錄。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 索引目錄的完整路徑。
    pub fn dir(&self) -> PathBuf {
        self.root.join(DIR_NAME)
    }

    /// 索引資料庫的完整路徑。
    pub fn db_path(&self) -> PathBuf {
        self.dir().join(DB_NAME)
    }

    /// 設定檔的完整路徑。
    pub fn config_path(&self) -> PathBuf {
        self.dir().join(CONFIG_NAME)
    }

    /// 將路徑轉為相對專案根目錄的形式，超出範圍時回 `None`。
    ///
    /// 資料庫一律儲存相對路徑，索引才能在不同機器之間共用。
    pub fn relativize<'a>(&self, path: &'a Path) -> Option<&'a Path> {
        path.strip_prefix(&self.root).ok()
    }
}

/// 取得絕對路徑。
///
/// 路徑尚未存在時 `canonicalize` 會失敗，改以工作目錄補齊，讓錯誤訊息
/// 仍能顯示完整路徑。
fn normalize(path: &Path) -> Result<PathBuf> {
    if let Ok(p) = path.canonicalize() {
        return Ok(strip_verbatim(p));
    }
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()?.join(path))
}

/// 去除 Windows canonicalize 產生的 `\\?\` 前綴。
fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => p,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 建立帶有 repo 邊界的暫存目錄。
    ///
    /// 暫存目錄位於使用者家目錄底下，而家目錄可能自己就有索引目錄。
    /// 沒有 `.git` 邊界時，「找不到索引」的測試會撞到那份索引。
    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codegraph-project-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        dir
    }

    #[test]
    fn create_makes_the_directory_and_a_config_template() {
        let root = tmpdir("create");
        let p = Project::create(&root).unwrap();

        assert!(p.dir().is_dir());
        assert!(p.config_path().is_file());
        assert!(
            std::fs::read_to_string(p.config_path())
                .unwrap()
                .contains("[index]")
        );
        assert_eq!(p.db_path().file_name().unwrap(), DB_NAME);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn create_is_idempotent_and_never_clobbers_an_edited_config() {
        let root = tmpdir("idempotent");
        let p = Project::create(&root).unwrap();
        std::fs::write(p.config_path(), "# 使用者改過的設定\n").unwrap();

        let again = Project::create(&root).unwrap();
        assert_eq!(p, again);
        assert_eq!(
            std::fs::read_to_string(again.config_path()).unwrap(),
            "# 使用者改過的設定\n",
            "重跑 init 蓋掉了使用者的設定"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn discover_walks_up_from_a_nested_directory() {
        let root = tmpdir("discover");
        Project::create(&root).unwrap();
        let nested = root.join("src").join("deep").join("deeper");
        std::fs::create_dir_all(&nested).unwrap();

        let found = Project::discover(&nested).unwrap();
        assert_eq!(found.root(), normalize(&root).unwrap());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn discover_reports_not_indexed_instead_of_failing_hard() {
        let root = tmpdir("bare");
        let err = Project::discover(&root).unwrap_err();
        assert!(
            matches!(err, CgError::NotIndexed { .. }),
            "沒有索引應該是可回復的狀況，實際是 {err:?}"
        );
        assert!(err.is_recoverable());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn discover_stops_at_the_repo_boundary() {
        let outer = tmpdir("boundary");
        Project::create(&outer).unwrap();

        let inner = outer.join("vendor").join("nested-repo");
        std::fs::create_dir_all(inner.join(".git")).unwrap();

        let err = Project::discover(&inner).unwrap_err();
        assert!(
            matches!(err, CgError::NotIndexed { .. }),
            "往上找越過了 .git 邊界，撿到上層 repo 的索引"
        );

        // 同一個位置若沒有 .git，就會沿用上層的索引。
        std::fs::remove_dir_all(inner.join(".git")).unwrap();
        assert_eq!(
            Project::discover(&inner).unwrap().root(),
            normalize(&outer).unwrap()
        );

        std::fs::remove_dir_all(&outer).ok();
    }

    #[test]
    fn discover_still_matches_at_the_boundary_directory_itself() {
        let root = tmpdir("at-boundary");
        Project::create(&root).unwrap();
        let nested = root.join("src");
        std::fs::create_dir_all(&nested).unwrap();

        assert_eq!(
            Project::discover(&nested).unwrap().root(),
            normalize(&root).unwrap()
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn is_initialized_reflects_the_directory() {
        let root = tmpdir("flag");
        assert!(!Project::is_initialized(&root));
        Project::create(&root).unwrap();
        assert!(Project::is_initialized(&root));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn paths_are_stored_relative_to_the_root() {
        let root = tmpdir("relativize");
        let p = Project::create(&root).unwrap();

        let inside = p.root().join("src").join("main.rs");
        assert_eq!(
            p.relativize(&inside),
            Some(Path::new("src").join("main.rs").as_path())
        );
        assert_eq!(
            p.relativize(Path::new("/somewhere/else.rs")),
            None,
            "專案外的路徑不該被當成相對路徑"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn normalize_handles_paths_that_do_not_exist_yet() {
        let missing = std::env::temp_dir().join("codegraph-does-not-exist-xyz");
        let n = normalize(&missing).unwrap();
        assert!(n.is_absolute());

        let relative = normalize(Path::new("some/relative/path")).unwrap();
        assert!(relative.is_absolute());
    }
}
