//! 子命令實作。
//!
//! 每個子命令回傳報告文字而不直接輸出，讓測試可以直接檢查內容。

pub mod edges;
pub mod explore;
pub mod index;
pub mod init;
pub mod outline;
pub mod serve;
pub mod status;
pub mod sync;

use std::path::{Path, PathBuf};

use crate::error::Result;

/// 取得起始路徑，未指定時使用工作目錄。
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
