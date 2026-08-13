//! CodeGraph 命令列介面。
//!
//! 只負責解析參數、呼叫 lib 並輸出結果。

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use code_graph::cli;

#[derive(Parser)]
#[command(
    name = "codegraph",
    version,
    about = "程式碼結構索引引擎",
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 建立索引目錄，不會開始索引
    Init {
        /// 專案根目錄，預設為工作目錄
        path: Option<PathBuf>,
    },
    /// 全量索引整個專案
    Index {
        /// 專案根目錄，預設從工作目錄往上尋找
        path: Option<PathBuf>,
    },
    /// 增量更新索引，只重新解析變更過的檔案
    Sync {
        /// 專案根目錄，預設從工作目錄往上尋找
        path: Option<PathBuf>,

        /// 常駐監看，存檔後自動更新
        #[arg(long)]
        watch: bool,
    },
    /// 顯示索引狀態
    Status {
        /// 專案根目錄，預設從工作目錄往上尋找
        path: Option<PathBuf>,
    },
    /// 依名字或問題查詢相關符號的原始碼
    Explore {
        /// 符號名、限定名、檔案路徑或自然語言
        query: Vec<String>,

        /// 專案根目錄，預設從工作目錄往上尋找
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// 列出呼叫這個符號的地方
    Callers {
        /// 符號名或限定名
        symbol: String,

        /// 專案根目錄，預設從工作目錄往上尋找
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// 列出這個符號呼叫了哪些地方
    Callees {
        /// 符號名或限定名
        symbol: String,

        /// 專案根目錄，預設從工作目錄往上尋找
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// 印出單一檔案的結構骨架
    Outline {
        /// 要分析的原始碼檔案
        file: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let report = match cli.command {
        Command::Init { path } => cli::init::run(path.as_deref())?,
        Command::Status { path } => cli::status::run(path.as_deref())?,
        Command::Explore { query, path } => cli::explore::run(&query.join(" "), path.as_deref())?,
        Command::Callers { symbol, path } => {
            cli::edges::run(&symbol, cli::edges::Direction::Callers, path.as_deref())?
        }
        Command::Callees { symbol, path } => {
            cli::edges::run(&symbol, cli::edges::Direction::Callees, path.as_deref())?
        }
        Command::Outline { file } => cli::outline::run(&file)?,
        Command::Index { path } => cli::index::run(path.as_deref())?,
        Command::Sync { path, watch } => {
            cli::sync::run(path.as_deref(), watch, |text| print!("{text}"))?
        }
    };

    print!("{report}");
    Ok(())
}
