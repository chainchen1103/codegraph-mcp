//! 錯誤模型。
//!
//! 兩層，界線清楚（ARCHITECTURE.md §8）：
//!
//! - lib 內部用 `anyhow` 加 context 往上拋。
//! - **MCP 邊界是唯一做分類的地方**：可回復的包成「成功 + 引導文字」，
//!   不可回復的才 `isError`。
//!
//! 為什麼這條契約重要：session 早期只要出現一兩次 `isError`，
//! agent 會整段停用這個工具，後面再也不呼叫，而且不會有任何訊號告訴你。
//! 見 DESIGN.md §5.3。

use std::fmt;
use std::path::PathBuf;

/// 檔案不在索引內的原因。要能對使用者交代清楚，
/// 「找不到」跟「被 ignore 掉了」是完全不同的處置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotIndexedReason {
    /// 被 ignore 規則排除。
    Ignored,
    /// 副檔名沒有對應的 extractor。
    UnsupportedExtension,
    /// 超過單檔大小上限。
    TooLarge,
    /// 檔案存在但這次索引之後才建立。
    NotYetIndexed,
}

impl NotIndexedReason {
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

#[derive(Debug)]
pub enum CgError {
    // ---- 可回復：MCP 層轉成「成功 + 引導文字」 ----
    /// 這個路徑（含所有上層目錄）沒有 `.codegraph/`。
    /// 在 monorepo 中只有子專案有索引是常態，這不是錯誤。
    NotIndexed { path: PathBuf },

    /// 查不到符號。一定要附候選清單，否則呼叫端只能去讀檔。
    SymbolNotFound {
        query: String,
        candidates: Vec<String>,
    },

    /// 檔案存在但不在索引內。
    FileNotIndexed {
        path: PathBuf,
        reason: NotIndexedReason,
    },

    // ---- 不可回復：MCP 層轉成 isError ----
    /// 安全拒絕：路徑逃逸出專案根目錄等。
    PathRefused { path: PathBuf },

    /// DB 損毀或 schema 不相容。
    Corrupt { detail: String },

    /// 底層 I/O 失敗。
    Io(std::io::Error),

    /// SQLite 失敗。
    Sqlite(rusqlite::Error),
}

impl CgError {
    /// 這個錯誤是不是「可回復」的。
    ///
    /// **這是 MCP 回應形狀的唯一判準**，`mcp/shape.rs` 依此決定
    /// 要包成成功外殼還是 `isError`。契約測試會逐項釘住每個變體。
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
                write!(f, "{} 不在索引內（{}）", path.display(), reason.as_str())
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

    /// 這個測試是 DESIGN.md §5.3 契約的第一道防線。
    /// 新增變體時編譯器會強迫 `is_recoverable` 補上分支，
    /// 這裡再確認分類沒有被改錯邊。
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

    /// 每個變體都要有看得懂的訊息。可回復的那三個會被原文送到 agent 面前
    /// 當作引導文字（DESIGN.md §5.3），所以不能是 `Debug` 那種格式。
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
        // 理由要能比較——重試邏輯會依理由決定要不要再試一次。
        assert_eq!(Ignored, Ignored);
        assert_ne!(Ignored, TooLarge);
    }

    /// 只有包了底層錯誤的變體才有 `source`。這關係到 `anyhow` 印出來的
    /// 錯誤鏈完不完整——少了 source，使用者只看得到「I/O 失敗」四個字。
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

    /// `?` 要能把底層錯誤自動轉成 `CgError`，否則每個呼叫點都得手動 map_err。
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

        // 成功路徑也要走一次，確認 `?` 沒有把正常結果一起吃掉。
        assert!(read("Cargo.toml").unwrap().contains("code_graph"));
        assert_eq!(query(false).unwrap(), "ok");
    }
}
