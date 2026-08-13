//! 回應形狀契約（DESIGN §5.3）。
//!
//! 這個 Stage 最容易做錯的不是協定，是形狀。session 早期出現一兩個
//! `isError`，agent 會整段停用工具，而且不會說。
//!
//! 因此規則只有一條：**操作本身沒出錯，就是成功。** 沒有索引、查不到
//! 符號、檔案不在索引內都屬於「先決條件還沒滿足」，回成功外殼加引導
//! 文字。只有安全拒絕與真實故障才是 `isError`。
//!
//! 這個判準不在這裡重新定義，直接用 [`CgError::is_recoverable`]，兩處
//! 各寫一份遲早會分岔。

use rmcp::model::{CallToolResult, ContentBlock};

use crate::error::{CgError, Result};

/// 真實故障時附上的註記。
///
/// 暫時性的失敗（檔案鎖、I/O）重試一次常常就好了；講清楚可以重試，
/// agent 才不會直接放棄整個工具。
const RETRY_NOTE: &str = "這是一次性故障，可重試一次。";

/// 把工具結果轉成 MCP 回應。
pub fn outcome(result: Result<String>) -> CallToolResult {
    match result {
        Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
        Err(e) if e.is_recoverable() => {
            CallToolResult::success(vec![ContentBlock::text(guidance(&e))])
        }
        Err(e) => CallToolResult::error(vec![ContentBlock::text(failure(&e))]),
    }
}

/// 可回復狀況的引導文字：講清楚現在怎麼了，以及下一步是什麼。
fn guidance(e: &CgError) -> String {
    match e {
        CgError::NotIndexed { path } => format!(
            "{} 及其上層目錄沒有 .codegraph/，這個路徑還沒有索引。\n\
             可以用 projectPath 指向工作區裡已經索引的目錄；\
             monorepo 只有部分子專案有索引是常見情況。\n\
             要建立索引請由使用者執行 codegraph index，本工具不會自行建立。\n",
            path.display()
        ),
        CgError::SymbolNotFound { query, candidates } => {
            let mut out = format!("索引裡沒有 `{query}`。\n");
            if candidates.is_empty() {
                out.push_str("也沒有相近的名稱。換個名字，或用 explore 問一個問題。\n");
            } else {
                out.push_str("相近的名稱：\n");
                for name in candidates {
                    out.push_str(&format!("  {name}\n"));
                }
            }
            out
        }
        CgError::FileNotIndexed { path, reason } => format!(
            "{} 不在索引內：{}。\n這個檔案要用 Read 讀，索引幫不上忙。\n",
            path.display(),
            reason.as_str()
        ),
        // 不可回復的錯誤不會走到這裡，但真的走到了也不能謊稱成功。
        other => format!("{other}\n"),
    }
}

/// 真實故障與安全拒絕的文字。
///
/// 安全拒絕是確定的結果，重試不會變；只有故障才附重試註記。
fn failure(e: &CgError) -> String {
    match e {
        CgError::PathRefused { .. } => format!("{e}\n"),
        _ => format!("{e}\n{RETRY_NOTE}\n"),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::error::NotIndexedReason;

    /// 取出回應的文字，方便斷言。
    fn text(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect()
    }

    fn is_error(result: &CallToolResult) -> bool {
        result.is_error.unwrap_or(false)
    }

    #[test]
    fn a_successful_call_passes_the_text_through() {
        let r = outcome(Ok("hello".to_string()));
        assert!(!is_error(&r));
        assert_eq!(text(&r), "hello");
    }

    /// 沒有索引是成功外殼，而且要指出 projectPath 這條路。
    #[test]
    fn a_missing_index_is_a_success_shell() {
        let r = outcome(Err(CgError::NotIndexed {
            path: PathBuf::from("/repo/sub"),
        }));
        assert!(!is_error(&r));

        let msg = text(&r);
        assert!(msg.contains("projectPath"), "{msg}");
        assert!(msg.contains(".codegraph"), "{msg}");
        assert!(msg.contains("不會自行建立"), "{msg}");
    }

    #[test]
    fn a_missing_symbol_is_a_success_shell_with_candidates() {
        let r = outcome(Err(CgError::SymbolNotFound {
            query: "opne".into(),
            candidates: vec!["open".into(), "open_ro".into()],
        }));
        assert!(!is_error(&r));

        let msg = text(&r);
        assert!(msg.contains("open_ro"), "{msg}");
    }

    #[test]
    fn a_missing_symbol_without_candidates_still_offers_a_next_step() {
        let r = outcome(Err(CgError::SymbolNotFound {
            query: "nope".into(),
            candidates: vec![],
        }));
        assert!(!is_error(&r));
        assert!(text(&r).contains("explore"), "{}", text(&r));
    }

    #[test]
    fn a_file_outside_the_index_is_a_success_shell_that_explains_why() {
        let r = outcome(Err(CgError::FileNotIndexed {
            path: PathBuf::from("/repo/a.png"),
            reason: NotIndexedReason::UnsupportedExtension,
        }));
        assert!(!is_error(&r));
        assert!(text(&r).contains("副檔名不支援"), "{}", text(&r));
    }

    /// 安全拒絕是唯一沒有重試餘地的錯誤。
    #[test]
    fn a_security_refusal_is_an_error_without_a_retry_note() {
        let r = outcome(Err(CgError::PathRefused {
            path: PathBuf::from("/etc/passwd"),
        }));
        assert!(is_error(&r));

        let msg = text(&r);
        assert!(msg.contains("拒絕"), "{msg}");
        assert!(!msg.contains("可重試"), "安全拒絕不該邀請重試：{msg}");
    }

    #[test]
    fn a_real_failure_is_an_error_with_a_retry_note() {
        for e in [
            CgError::Corrupt {
                detail: "bad header".into(),
            },
            CgError::Io(std::io::Error::other("boom")),
            CgError::Sqlite(rusqlite::Error::InvalidQuery),
        ] {
            let r = outcome(Err(e));
            assert!(is_error(&r));
            assert!(text(&r).contains("可重試一次"), "{}", text(&r));
        }
    }

    /// 形狀只依 `is_recoverable` 判斷，兩邊不能各說各話。
    #[test]
    fn the_shape_follows_the_recoverable_classification() {
        let cases = [
            CgError::NotIndexed {
                path: PathBuf::from("/x"),
            },
            CgError::SymbolNotFound {
                query: "x".into(),
                candidates: vec![],
            },
            CgError::FileNotIndexed {
                path: PathBuf::from("/x"),
                reason: NotIndexedReason::Ignored,
            },
            CgError::PathRefused {
                path: PathBuf::from("/x"),
            },
            CgError::Corrupt { detail: "x".into() },
        ];

        for e in cases {
            let recoverable = e.is_recoverable();
            let r = outcome(Err(e));
            assert_eq!(is_error(&r), !recoverable);
        }
    }

    /// 引導文字永遠不是空的，空回應等於沒回答。
    #[test]
    fn guidance_is_never_empty() {
        let msg = guidance(&CgError::Corrupt {
            detail: "bad header".into(),
        });
        assert!(msg.contains("bad header"), "{msg}");
    }
}
