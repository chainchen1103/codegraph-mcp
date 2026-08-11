//! 子命令實作。
//!
//! 每個子命令都是 `run(...) -> Result<String>`：**回傳報告文字，不自己印**。
//! 這樣測試可以直接斷言輸出內容，不必去解析 stdout；`main.rs` 只負責印。

pub mod init;
pub mod outline;
pub mod status;

use std::path::{Path, PathBuf};

use crate::error::Result;

/// 沒給路徑就用現在的工作目錄。
pub(crate) fn resolve_start(path: Option<&Path>) -> Result<PathBuf> {
    match path {
        Some(p) => Ok(p.to_path_buf()),
        None => Ok(std::env::current_dir()?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_path_wins_over_the_working_directory() {
        let p = Path::new("some/where");
        assert_eq!(resolve_start(Some(p)).unwrap(), PathBuf::from("some/where"));
    }

    #[test]
    fn no_path_falls_back_to_the_working_directory() {
        assert_eq!(
            resolve_start(None).unwrap(),
            std::env::current_dir().unwrap()
        );
    }
}
