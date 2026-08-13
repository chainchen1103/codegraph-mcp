//! `serve` 子命令：以 stdio 提供 MCP 服務。
//!
//! stdio 上跑的是協定本身，任何多印的一個字都會把它弄壞，因此這個模式
//! 下不對 stdout 輸出任何東西。

use std::path::{Path, PathBuf};

use rmcp::ServiceExt;
use rmcp::transport::stdio;

use crate::error::{CgError, Result};
use crate::mcp::Server;

/// 啟動 MCP server；`print_config` 為真時只印設定片段就返回。
pub fn run(print_config: bool) -> Result<String> {
    if print_config {
        return Ok(config_snippet(&exe_path()));
    }

    serve()?;
    Ok(String::new())
}

/// 接上 stdio 並跑到連線結束。
fn serve() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let (input, output) = stdio();
    runtime.block_on(serve_on(input, output))
}

/// 在給定的一對串流上提供服務，直到對方關閉連線。
///
/// 傳輸抽出來是為了讓服務本身測得到：stdio 是整個行程共用的，測試接管
/// 它會把測試框架自己的輸出一起吃掉。
async fn serve_on<R, W>(input: R, output: W) -> Result<()>
where
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
    W: tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    let service = Server::new()
        .serve((input, output))
        .await
        .map_err(transport_failed)?;
    service.waiting().await.map_err(transport_failed)?;
    Ok(())
}

/// 手動設定片段。
///
/// Claude Code 與 Cursor 都吃這個形狀，貼進各自的設定檔即可。
fn config_snippet(exe: &Path) -> String {
    format!(
        "MCP 設定：\n\
         \n\
         {{\n\
         \x20 \"mcpServers\": {{\n\
         \x20   \"codegraph\": {{\n\
         \x20     \"command\": {},\n\
         \x20     \"args\": [\"serve\"]\n\
         \x20   }}\n\
         \x20 }}\n\
         }}\n",
        quote(&exe.display().to_string())
    )
}

/// 目前執行檔的路徑，取不到時退回命令名。
fn exe_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("codegraph"))
}

/// 包成 JSON 字串。Windows 的路徑帶反斜線，不跳脫就不是合法 JSON。
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn transport_failed(e: impl std::fmt::Display) -> CgError {
    CgError::Io(std::io::Error::other(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_config_snippet_names_the_executable_and_the_subcommand() {
        let out = run(true).unwrap();
        assert!(out.contains("mcpServers"), "{out}");
        assert!(out.contains("\"codegraph\""), "{out}");
        assert!(out.contains("[\"serve\"]"), "{out}");
    }

    /// Windows 的路徑帶反斜線，片段必須是合法 JSON 才貼得進去。
    #[test]
    fn a_windows_path_stays_valid_json() {
        let snippet = config_snippet(Path::new(r"C:\tools\codegraph.exe"));
        let json = snippet.split_once("{").unwrap().1;
        let json = format!("{{{json}");

        let parsed: serde_json::Value = serde_json::from_str(json.trim()).unwrap();
        assert_eq!(
            parsed["mcpServers"]["codegraph"]["command"],
            r"C:\tools\codegraph.exe"
        );
    }

    #[test]
    fn quoting_escapes_what_json_requires() {
        assert_eq!(quote(r"a\b"), r#""a\\b""#);
        assert_eq!(quote(r#"a"b"#), r#""a\"b""#);
    }

    #[test]
    fn the_executable_path_always_resolves_to_something() {
        assert!(!exe_path().as_os_str().is_empty());
    }

    /// 服務要真的答得出 initialize，而且對方關閉之後要自己結束。
    #[tokio::test]
    async fn the_service_answers_initialize_and_stops_when_the_client_leaves() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let (client, server) = tokio::io::duplex(8192);
        let (server_in, server_out) = tokio::io::split(server);
        let serving = tokio::spawn(serve_on(server_in, server_out));

        let (client_in, mut client_out) = tokio::io::split(client);
        client_out
            .write_all(
                b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":\
                  {\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\
                  \"clientInfo\":{\"name\":\"t\",\"version\":\"1\"}}}\n",
            )
            .await
            .unwrap();

        let mut lines = BufReader::new(client_in).lines();
        let reply = lines.next_line().await.unwrap().expect("沒有回應");
        assert!(reply.contains("instructions"), "{reply}");

        // 關掉連線，服務應該跟著收工。
        drop(lines);
        drop(client_out);
        serving.await.unwrap().unwrap();
    }

    #[test]
    fn transport_failures_are_reported_as_io() {
        let e = transport_failed("broken pipe");
        assert!(!e.is_recoverable());
        assert!(e.to_string().contains("broken pipe"));
    }
}
