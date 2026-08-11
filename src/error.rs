//! 錯誤型別。
//!
//! 錯誤分成可回復與不可回復兩類。可回復的表示使用者只是還沒做某件事，
//! 呼叫端應該把訊息當成引導顯示；不可回復的才是真正的失敗。

use std::fmt;
use std::path::PathBuf;

/// 檔案不在索引內的原因。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotIndexedReason {
    /// 被 ignore 規則排除。
    Ignored,
    /// 副檔名沒有對應的抽取器。
    UnsupportedExtension,
    /// 超過單檔大小上限。
    TooLarge,
    /// 檔案存在，但索引建立之後才出現。
    NotYetIndexed,
}

impl NotIndexedReason {
    /// 給使用者看的說明。
    pub fn as_str(&self) -> &'static str {
        use NotIndexedReason::*;
        match self {
            Ignored => "被 ignore 規則排除",
            UnsupportedExtension => "副檔名不支援",
            TooLarge => "超過單檔大小上限",
            NotYetIndexed => "尚未索引",
        }
    }
}

/// 本 crate 的錯誤型別。
#[derive(Debug)]
pub enum CgError {
    /// 指定路徑與其上層目錄都沒有索引。
    ///
    /// 在 monorepo 中只有部分子專案建立索引是常見情況。
    NotIndexed { path: PathBuf },

    /// 查不到符號。附上最接近的候選，讓呼叫端不必再自行搜尋。
    SymbolNotFound {
        query: String,
        candidates: Vec<String>,
    },

    /// 檔案存在但不在索引內。
    FileNotIndexed {
        path: PathBuf,
        reason: NotIndexedReason,
    },

    /// 路徑超出專案範圍，拒絕存取。
    PathRefused { path: PathBuf },

    /// 索引損毀，或 schema 版本不相容。
    Corrupt { detail: String },

    /// 底層 I/O 失敗。
    Io(std::io::Error),

    /// SQLite 失敗。
    Sqlite(rusqlite::Error),
}

impl CgError {
    /// 是否為可回復的狀況。
    ///
    /// 可回復表示操作本身沒有出錯，只是先決條件還沒滿足。介面層依此
    /// 決定要回傳引導訊息還是錯誤。
    pub fn is_recoverable(&self) -> bool {
        use CgError::*;
        match self {
            NotIndexed { .. } | SymbolNotFound { .. } | FileNotIndexed { .. } => true,
            PathRefused { .. } | Corrupt { .. } | Io(_) | Sqlite(_) => false,
        }
    }
}

impl fmt::Display for CgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use CgError::*;
        match self {
            NotIndexed { path } => {
                write!(f, "{} 及其上層目錄找不到 .codegraph/", path.display())
            }
            SymbolNotFound { query, candidates } => {
                write!(f, "找不到符號 `{query}`")?;
                if !candidates.is_empty() {
                    write!(f, "；相近的有：{}", candidates.join(", "))?;
                }
                Ok(())
            }
            FileNotIndexed { path, reason } => {
                write!(f, "{} 不在索引內：{}", path.display(), reason.as_str())
            }
            PathRefused { path } => write!(f, "拒絕存取專案範圍外的路徑：{}", path.display()),
            Corrupt { detail } => write!(f, "索引損毀：{detail}"),
            Io(e) => write!(f, "I/O 失敗：{e}"),
            Sqlite(e) => write!(f, "SQLite 失敗：{e}"),
        }
    }
}

impl std::error::Error for CgError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CgError::Io(e) => Some(e),
            CgError::Sqlite(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for CgError {
    fn from(e: std::io::Error) -> Self {
        CgError::Io(e)
    }
}

impl From<rusqlite::Error> for CgError {
    fn from(e: rusqlite::Error) -> Self {
        CgError::Sqlite(e)
    }
}

/// 本 crate 的預設 `Result`。
pub type Result<T> = std::result::Result<T, CgError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recoverable_classification_matches_contract() {
        let recoverable = [
            CgError::NotIndexed {
                path: PathBuf::from("/x"),
            },
            CgError::SymbolNotFound {
                query: "foo".into(),
                candidates: vec![],
            },
            CgError::FileNotIndexed {
                path: PathBuf::from("/x"),
                reason: NotIndexedReason::Ignored,
            },
        ];
        for e in &recoverable {
            assert!(e.is_recoverable(), "{e} 應該是可回復的");
        }

        let fatal = [
            CgError::PathRefused {
                path: PathBuf::from("/etc/passwd"),
            },
            CgError::Corrupt {
                detail: "bad header".into(),
            },
        ];
        for e in &fatal {
            assert!(!e.is_recoverable(), "{e} 不該被當成可回復的");
        }
    }

    #[test]
    fn symbol_not_found_shows_candidates() {
        let e = CgError::SymbolNotFound {
            query: "opne".into(),
            candidates: vec!["open".into(), "open_ro".into()],
        };
        let msg = e.to_string();
        assert!(msg.contains("opne"));
        assert!(msg.contains("open_ro"));
    }

    #[test]
    fn symbol_not_found_without_candidates_omits_the_suffix() {
        let e = CgError::SymbolNotFound {
            query: "nope".into(),
            candidates: vec![],
        };
        let msg = e.to_string();
        assert!(msg.contains("nope"));
        assert!(!msg.contains("相近的有"), "沒有候選時不該印空清單：{msg}");
    }

    #[test]
    fn every_variant_renders_a_useful_message() {
        let cases: Vec<(CgError, &str)> = vec![
            (
                CgError::NotIndexed {
                    path: PathBuf::from("/repo/sub"),
                },
                ".codegraph",
            ),
            (
                CgError::SymbolNotFound {
                    query: "foo".into(),
                    candidates: vec![],
                },
                "foo",
            ),
            (
                CgError::FileNotIndexed {
                    path: PathBuf::from("/repo/a.png"),
                    reason: NotIndexedReason::UnsupportedExtension,
                },
                "副檔名不支援",
            ),
            (
                CgError::PathRefused {
                    path: PathBuf::from("/etc/passwd"),
                },
                "拒絕",
            ),
            (
                CgError::Corrupt {
                    detail: "bad header".into(),
                },
                "bad header",
            ),
            (
                CgError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "no such file",
                )),
                "no such file",
            ),
            (CgError::Sqlite(rusqlite::Error::InvalidQuery), "SQLite"),
        ];
        for (e, needle) in cases {
            let msg = e.to_string();
            assert!(!msg.is_empty());
            assert!(msg.contains(needle), "訊息 `{msg}` 少了 `{needle}`");
        }
    }

    #[test]
    fn not_indexed_reasons_all_have_labels() {
        use NotIndexedReason::*;
        for r in [Ignored, UnsupportedExtension, TooLarge, NotYetIndexed] {
            assert!(!r.as_str().is_empty());
        }
        assert_eq!(Ignored, Ignored);
        assert_ne!(Ignored, TooLarge);
    }

    #[test]
    fn only_wrapped_errors_expose_a_source() {
        use std::error::Error;

        let io = CgError::from(std::io::Error::other("boom"));
        assert!(io.source().is_some());

        let sql = CgError::from(rusqlite::Error::InvalidQuery);
        assert!(sql.source().is_some());

        let plain = CgError::Corrupt { detail: "x".into() };
        assert!(plain.source().is_none());
    }

    #[test]
    fn question_mark_converts_underlying_errors() {
        fn read(path: &str) -> Result<String> {
            let s = std::fs::read_to_string(path)?;
            Ok(s)
        }
        fn query(fail: bool) -> Result<&'static str> {
            if fail {
                Err(rusqlite::Error::InvalidQuery)?;
            }
            Ok("ok")
        }

        assert!(matches!(
            read("這個檔案不存在-codegraph-test"),
            Err(CgError::Io(_))
        ));
        assert!(matches!(query(true), Err(CgError::Sqlite(_))));

        assert!(read("Cargo.toml").unwrap().contains("code_graph"));
        assert_eq!(query(false).unwrap(), "ok");
    }
}
