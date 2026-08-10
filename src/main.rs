//! CodeGraph CLI。
//!
//! 這一層刻意做薄：只負責解析參數、呼叫 lib、印出結果。
//! MCP server（Stage 9）會呼叫同一組 lib 函數，兩邊行為才不會分岔。

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use code_graph::cli;

#[derive(Parser)]
#[command(
    name = "codegraph",
    version,
    about = "程式碼結構索引引擎 —— 讓 AI agent 用一次查詢取代讀檔"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 在指定目錄建立 .codegraph/（不會自動開始索引）
    Init {
        /// 專案根目錄，預設為現在的工作目錄
        path: Option<PathBuf>,
    },
    /// 全量索引整個專案
    Index {
        /// 專案根目錄，預設從現在的工作目錄往上找 .codegraph/
        path: Option<PathBuf>,
    },
    /// 顯示索引狀態與新鮮度
    Status {
        /// 專案根目錄，預設從現在的工作目錄往上找 .codegraph/
        path: Option<PathBuf>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let report = match cli.command {
        Command::Init { path } => cli::init::run(path.as_deref())?,
        Command::Status { path } => cli::status::run(path.as_deref())?,
        Command::Index { path } => todo!("Stage 3: cli::index（path = {path:?}）"),
    };

    print!("{report}");
    Ok(())
}
