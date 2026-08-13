//! MCP 回應形狀的驗收（DESIGN §5.3）。
//!
//! 這一份釘的是形狀，不是內容。session 早期出現一兩個 `isError`，agent
//! 會整段停用工具而且不會說，所以每一種 `CgError` 對應到成功外殼還是
//! `isError`，必須逐項寫死在測試裡。

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use code_graph::error::{CgError, NotIndexedReason};
use code_graph::mcp::session::Session;
use code_graph::mcp::{shape, tools};

use common::Fixture;

/// 回應的文字。
fn text(result: &rmcp::model::CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect()
}

fn is_error(result: &rmcp::model::CallToolResult) -> bool {
    result.is_error.unwrap_or(false)
}

// ---------------------------------------------------------------- 成功外殼

/// 專案未索引：成功，內容指出可以用 projectPath 指向已索引的專案。
#[test]
fn an_unindexed_project_is_a_success_shell() {
    let dir = std::env::temp_dir().join(format!("codegraph-mcp-bare-{}", std::process::id()));
    std::fs::create_dir_all(dir.join(".git")).unwrap();

    let mut session = Session::new();
    for result in [
        shape::outcome(tools::explore(Some(&dir), "anything", &mut session)),
        shape::outcome(tools::node(Some(&dir), "anything")),
        shape::outcome(tools::status(Some(&dir))),
    ] {
        assert!(!is_error(&result), "未索引不該是錯誤：{}", text(&result));
        assert!(text(&result).contains("projectPath"), "{}", text(&result));
    }

    std::fs::remove_dir_all(&dir).ok();
}

/// 找不到符號：成功，內容是最接近的候選清單。
#[test]
fn a_missing_symbol_is_a_success_shell_with_candidates() {
    let f = Fixture::indexed("mcp-nosym", &[("src/a.rs", "pub fn opened() {}\n")]);

    let result = shape::outcome(tools::node(Some(f.project.root()), "opend"));
    assert!(!is_error(&result));

    let msg = text(&result);
    assert!(msg.contains("opend"), "{msg}");
    assert!(msg.contains("opened"), "候選清單不見了：{msg}");
}

/// 檔案不在索引內：成功，說明原因。
#[test]
fn a_file_outside_the_index_is_a_success_shell_that_explains_why() {
    for reason in [
        NotIndexedReason::Ignored,
        NotIndexedReason::UnsupportedExtension,
        NotIndexedReason::TooLarge,
        NotIndexedReason::NotYetIndexed,
    ] {
        let result = shape::outcome(Err(CgError::FileNotIndexed {
            path: PathBuf::from("/repo/a.png"),
            reason: reason.clone(),
        }));
        assert!(!is_error(&result), "{reason:?} 不該是錯誤");
        assert!(text(&result).contains(reason.as_str()), "{reason:?}");
    }
}

// ------------------------------------------------------------------ isError

/// 安全拒絕：`isError`，而且不邀請重試——重試不會有不同結果。
#[test]
fn a_security_refusal_is_an_error() {
    let result = shape::outcome(Err(CgError::PathRefused {
        path: PathBuf::from("/etc/passwd"),
    }));

    assert!(is_error(&result));
    assert!(!text(&result).contains("可重試"), "{}", text(&result));
}

/// 真實故障：`isError`，並附「可重試一次」註記。
#[test]
fn a_real_failure_is_an_error_marked_retryable() {
    for e in [
        CgError::Corrupt {
            detail: "bad header".into(),
        },
        CgError::Io(std::io::Error::other("boom")),
        CgError::Sqlite(rusqlite::Error::InvalidQuery),
    ] {
        let result = shape::outcome(Err(e));
        assert!(is_error(&result));
        assert!(text(&result).contains("可重試一次"), "{}", text(&result));
    }
}

/// 完整對照表：可回復的一律成功外殼，其餘一律 `isError`。
#[test]
fn every_error_lands_on_the_side_its_classification_says() {
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
        CgError::Io(std::io::Error::other("x")),
        CgError::Sqlite(rusqlite::Error::InvalidQuery),
    ];

    for e in cases {
        let recoverable = e.is_recoverable();
        let result = shape::outcome(Err(e));
        assert_eq!(is_error(&result), !recoverable);
        assert!(!text(&result).is_empty(), "回應不能是空的");
    }
}

// -------------------------------------------------------- 真的跑起來的 server

/// 一個跑在子行程上的 server，離開作用域時收掉。
struct Serving {
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
}

impl Serving {
    /// 在 `cwd` 啟動 `codegraph serve`。
    fn start(cwd: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_codegraph"))
            .arg("serve")
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("啟動不了 codegraph serve");

        let reader = BufReader::new(child.stdout.take().unwrap());
        Self { child, reader }
    }

    /// 送一行 JSON-RPC。
    fn send(&mut self, message: &serde_json::Value) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    }

    /// 讀一行回應。
    fn recv(&mut self) -> serde_json::Value {
        let mut line = String::new();
        self.reader.read_line(&mut line).expect("讀不到回應");
        assert!(!line.trim().is_empty(), "server 沒有回應就關掉了");
        serde_json::from_str(&line).expect("回應不是 JSON")
    }

    fn initialize(&mut self) -> serde_json::Value {
        self.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "acceptance", "version": "1" }
            }
        }));
        self.recv()
    }
}

impl Drop for Serving {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 未索引的根目錄下工具照樣曝光。
///
/// 隱藏會打壞 monorepo（只有子專案有索引）與「session 開始之後才建
/// 索引」的情境。
#[test]
fn the_tools_are_exposed_even_without_an_index() {
    let dir = std::env::temp_dir().join(format!("codegraph-mcp-serve-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".git")).unwrap();

    let mut serving = Serving::start(&dir);
    let hello = serving.initialize();
    assert!(
        hello["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("explore"),
        "initialize 沒有帶指引：{hello}"
    );

    serving.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    }));
    serving.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    }));

    let listed = serving.recv();
    let mut names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .expect("沒有工具清單")
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    names.sort();

    assert_eq!(names, ["explore", "node", "status"], "{listed}");

    drop(serving);
    std::fs::remove_dir_all(&dir).ok();
}

/// 冷啟動要夠快，否則 agent 那一側會先超時。
#[test]
fn a_cold_start_answers_within_the_budget() {
    let dir = std::env::temp_dir().join(format!("codegraph-mcp-cold-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".git")).unwrap();

    let started = Instant::now();
    let mut serving = Serving::start(&dir);
    let hello = serving.initialize();
    let elapsed = started.elapsed();

    assert!(hello["result"].is_object(), "{hello}");
    assert!(
        elapsed < Duration::from_millis(200),
        "冷啟動花了 {} ms",
        elapsed.as_millis()
    );

    drop(serving);
    std::fs::remove_dir_all(&dir).ok();
}
