//! 專案識別：找到 `.codegraph/` 在哪。
//!
//! 磁碟佈局見 ARCHITECTURE.md §9。

use std::path::{Path, PathBuf};

use crate::error::{CgError, Result};

/// 索引目錄的名字。
pub const DIR_NAME: &str = ".codegraph";

/// 本機可寫的現況圖。
pub const DB_NAME: &str = "graph.db";

/// 使用者設定。Stage 1 只產生範本，實際解析等到有設定要讀時再做
/// （ignore 規則在 Stage 3）。
pub const CONFIG_NAME: &str = "config.toml";

const CONFIG_TEMPLATE: &str = "\
# CodeGraph 專案設定
#
# 這個檔案目前只是範本——Stage 3（全量索引）才會開始讀它。
# 屆時支援的項目：
#   [index]  extra_ignore = [\"vendor/**\"]
#   [budget] override = { max_chars = 30000 }

[index]
extra_ignore = []
";

/// 一個已經初始化的專案。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    /// 專案根目錄，也就是 `.codegraph/` 的上一層。
    root: PathBuf,
}

impl Project {
    /// 從 `start` 往上找最近的 `.codegraph/`，**但不越過 repo 邊界**。
    ///
    /// 邊界規則：走到含有 `.git` 的目錄就停（該層本身仍會檢查）。
    /// 索引屬於某一個 repo；沒有這條界線的話，在一個未索引的專案裡執行
    /// 就會一路往上撞到家目錄的 `.codegraph/`，然後拿**別的專案的圖**
    /// 回答問題——而且不會有任何錯誤訊息。
    ///
    /// 找不到**不是程式錯誤**：monorepo 中只有子專案有索引是常態，
    /// 而且索引與否是使用者的決定。回 `NotIndexed`，由 MCP 層包成
    /// 成功外殼加引導文字（DESIGN.md §5.3）。
    pub fn discover(start: &Path) -> Result<Self> {
        let start = normalize(start)?;
        for dir in start.ancestors() {
            if dir.join(DIR_NAME).is_dir() {
                return Ok(Self {
                    root: dir.to_path_buf(),
                });
            }
            // `.git` 可能是目錄，也可能是 worktree 的一個檔案。
            if dir.join(".git").exists() {
                break;
            }
        }
        Err(CgError::NotIndexed { path: start })
    }

    /// 在 `root` 建立 `.codegraph/`。已經存在則原樣回傳——
    /// `init` 必須可以重複執行而不破壞既有索引。
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

    /// 這個路徑上（或其上層）是否已經有索引。
    pub fn is_initialized(root: &Path) -> bool {
        root.join(DIR_NAME).is_dir()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `.codegraph/` 本身。
    pub fn dir(&self) -> PathBuf {
        self.root.join(DIR_NAME)
    }

    pub fn db_path(&self) -> PathBuf {
        self.dir().join(DB_NAME)
    }

    pub fn config_path(&self) -> PathBuf {
        self.dir().join(CONFIG_NAME)
    }

    /// 把絕對路徑轉成相對專案根目錄的形式。
    ///
    /// DB 裡一律存相對路徑——絕對路徑會把「誰的機器、放在哪個目錄」
    /// 烙進索引，artifact 一換機器就全錯（DESIGN.md §1.1 的可分發前提）。
    pub fn relativize<'a>(&self, path: &'a Path) -> Option<&'a Path> {
        path.strip_prefix(&self.root).ok()
    }
}

/// 盡量取得絕對路徑。路徑不存在時 `canonicalize` 會失敗，
/// 此時退回「工作目錄 + 相對路徑」，讓錯誤訊息還是印得出完整路徑。
fn normalize(path: &Path) -> Result<PathBuf> {
    if let Ok(p) = path.canonicalize() {
        // Windows 的 canonicalize 會產生 \\?\ 前綴，印出來很醜，去掉。
        return Ok(strip_verbatim(p));
    }
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()?.join(path))
}

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

    /// 建一個**帶 repo 邊界**的暫存目錄。
    ///
    /// `.git` 是刻意放的：暫存目錄位在使用者家目錄底下，而家目錄可能
    /// 真的有一個 `.codegraph/`（開發機上就有）。沒有邊界的話，
    /// 「找不到索引」的測試會撞到那份索引而變成偶發性通過。
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

    /// 索引屬於某一個 repo。上層 repo 的索引不該被下層的 repo 撿去用——
    /// 拿別人的圖回答問題，比回答「沒有索引」糟得多。
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

        // 對照組：同一個位置若沒有 .git，就會沿用上層的索引。
        std::fs::remove_dir_all(inner.join(".git")).unwrap();
        assert_eq!(
            Project::discover(&inner).unwrap().root(),
            normalize(&outer).unwrap()
        );

        std::fs::remove_dir_all(&outer).ok();
    }

    /// 邊界那一層本身仍要被檢查——絕大多數專案的 `.codegraph/` 就是
    /// 跟 `.git/` 放在同一層。
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
