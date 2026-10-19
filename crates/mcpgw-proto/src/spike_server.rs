//! M0-04 spike: a minimal `rmcp` server with one hard-coded `echo` tool.
//! Throwaway; proves `rmcp` can be the client-facing MCP server.

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, ServerCapabilities, ServerConfig},
    schemars, tool, tool_handler, tool_router, ErrorData, ServerHandler,
};
use serde::Deserialize;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EchoArgs {
    /// Text to echo back.
    pub text: String,
}

#[derive(Clone)]
pub struct EchoServer {
    tool_router: ToolRouter<Self>,
}

impl Default for EchoServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router]
impl EchoServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Echo the given text back")]
    async fn echo(
        &self,
        Parameters(args): Parameters<EchoArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        Ok(CallToolResult::success(vec![ContentBlock::text(args.text)]))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for EchoServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }
}
