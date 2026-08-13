//! MCP server：把索引接到 Claude Code / Cursor 這類 agent 上。
//!
//! 這一層只做三件事：把參數轉成呼叫、把結果轉成回應形狀、記住這個
//! session 已經送過什麼。真正的查詢在 [`tools`]，形狀契約在 [`shape`]。

pub mod instructions;
pub mod session;
pub mod shape;
pub mod tools;

use std::sync::{Arc, Mutex};

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use session::Session;

/// `explore` 的參數。
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExploreArgs {
    /// 一個問題，或一串符號名／限定名／檔案路徑。
    pub query: String,
    /// 要查哪個專案，省略時用工作目錄。
    #[serde(default)]
    pub project_path: Option<String>,
}

/// `node` 的參數。
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NodeArgs {
    /// 符號名或限定名（`Type::method`）。
    pub name: String,
    /// 要查哪個專案，省略時用工作目錄。
    #[serde(default)]
    pub project_path: Option<String>,
}

/// `status` 的參數。
#[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StatusArgs {
    /// 要查哪個專案，省略時用工作目錄。
    #[serde(default)]
    pub project_path: Option<String>,
}

/// 一個 MCP 連線。
#[derive(Clone)]
pub struct Server {
    /// 這個連線送出過什麼，用來去重。
    session: Arc<Mutex<Session>>,
    tool_router: ToolRouter<Self>,
}

impl Server {
    pub fn new() -> Self {
        Self {
            session: Arc::new(Mutex::new(Session::new())),
            tool_router: Self::tool_router(),
        }
    }
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router(router = tool_router)]
impl Server {
    /// 主入口。
    #[tool(
        description = "取回相關符號的逐字帶行號原始碼，以及被指名符號之間的呼叫\
                       路徑。輸入可以是自然語言問句，或一串符號名／限定名／\
                       檔案路徑。"
    )]
    pub async fn explore(&self, args: Parameters<ExploreArgs>) -> CallToolResult {
        let Parameters(args) = args;
        // 鎖只在這一次呼叫內持有，中間沒有 await。
        let mut session = self.session.lock().expect("session 鎖毀損");
        shape::outcome(tools::explore(
            tools::path_arg(&args.project_path).as_deref(),
            &args.query,
            &mut session,
        ))
    }

    /// 深挖單一符號。
    #[tool(
        description = "回傳單一符號的完整 body（不裁切）與它的呼叫者、被呼叫者。\
                       名字有多個定義時全部回傳。"
    )]
    pub async fn node(&self, args: Parameters<NodeArgs>) -> CallToolResult {
        let Parameters(args) = args;
        shape::outcome(tools::node(
            tools::path_arg(&args.project_path).as_deref(),
            &args.name,
        ))
    }

    /// 索引狀態。
    #[tool(description = "索引的規模與新鮮度。")]
    pub async fn status(&self, args: Parameters<StatusArgs>) -> CallToolResult {
        let Parameters(args) = args;
        shape::outcome(tools::status(
            tools::path_arg(&args.project_path).as_deref(),
        ))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    /// 工具介面永遠曝光，即使當前根目錄沒有索引。
    ///
    /// 隱藏會打壞 monorepo（只有子專案有索引）與「session 開始之後才
    /// 建索引」的情境。安全性來自回應的形狀，不是靠藏工具。
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(instructions::INSTRUCTIONS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{cleanup, indexed_project, tmpdir};

    fn text(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect()
    }

    /// 只有三個工具，而且名字就是 agent 會打的那三個。
    #[test]
    fn exactly_three_tools_are_exposed() {
        let router = Server::tool_router();
        let mut names: Vec<String> = router
            .list_all()
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        names.sort();

        assert_eq!(names, ["explore", "node", "status"]);
    }

    /// 沒有索引的根目錄下工具照樣曝光。
    #[tokio::test]
    async fn tools_stay_exposed_without_an_index() {
        let dir = tmpdir("mcp-server-bare");
        let previous = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();

        let server = Server::new();
        assert_eq!(server.tool_router.list_all().len(), 3);

        let result = server.status(Parameters(StatusArgs::default())).await;
        assert_ne!(result.is_error, Some(true), "沒有索引不該是錯誤");
        assert!(text(&result).contains("projectPath"));

        std::env::set_current_dir(previous).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn the_three_tools_answer_over_a_real_index() {
        let p = indexed_project(
            "mcp-server",
            &[("src/a.rs", "pub fn target() {\n    1;\n}\n")],
        );
        let root = p.root().display().to_string();
        let server = Server::new();

        let explored = server
            .explore(Parameters(ExploreArgs {
                query: "target".into(),
                project_path: Some(root.clone()),
            }))
            .await;
        assert!(text(&explored).contains("pub fn target()"));

        let node = server
            .node(Parameters(NodeArgs {
                name: "target".into(),
                project_path: Some(root.clone()),
            }))
            .await;
        assert!(text(&node).contains("沒有呼叫者"));

        let status = server
            .status(Parameters(StatusArgs {
                project_path: Some(root),
            }))
            .await;
        assert!(text(&status).contains("符號      1"));

        cleanup(&p);
    }

    /// session 記在連線上，同一個 server 問兩次第二次只拿指標。
    #[tokio::test]
    async fn a_connection_remembers_what_it_sent() {
        let p = indexed_project("mcp-server-dedup", &[("src/a.rs", "pub fn target() {}\n")]);
        let root = p.root().display().to_string();
        let server = Server::new();

        let args = || {
            Parameters(ExploreArgs {
                query: "target".into(),
                project_path: Some(root.clone()),
            })
        };

        server.explore(args()).await;
        let again = server.explore(args()).await;
        assert!(text(&again).contains("稍早已送出"), "{}", text(&again));

        cleanup(&p);
    }

    #[test]
    fn initialize_carries_the_agent_instructions() {
        let info = Server::new().get_info();
        assert_eq!(
            info.instructions.as_deref(),
            Some(instructions::INSTRUCTIONS)
        );
        assert!(info.capabilities.tools.is_some());
    }
}
