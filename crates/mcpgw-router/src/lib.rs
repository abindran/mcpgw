//! `mcpgw-router` — aggregation, namespacing, meta-tools, policy checks and
//! the result pipeline.
//!
//! Exposes the aggregated backend tools to LLM clients. It never imports
//! `rmcp` directly; all protocol access goes through `mcpgw-proto`.

pub mod aggregate;
pub mod login;
pub mod meta;

use std::sync::Arc;

pub use aggregate::{
    MAX_TOOL_NAME, Scope, call_in, call_public, call_public_as, list_all_tools, list_tools_in,
    public_name, resolve, resolve_in, split_name,
};
use mcpgw_core::Mode;
use mcpgw_proto::front::{self, BoxFuture, FrontInfo, ToolProvider};
use mcpgw_runtime::{BackendManager, JsonObject, ProtoError};

/// Client-facing protocol version this side presents (via `mcpgw-proto`).
pub fn client_facing_version() -> &'static str {
    mcpgw_proto::version::CLIENT_FACING
}

/// How the gateway presents tools.
#[derive(Clone, Default)]
pub struct GatewayOptions {
    /// Starts logins for calls that need one (M2-06); none in stdio mode.
    pub login: Option<login::LoginLink>,
    /// `direct`: every backend tool; `meta`: only the meta-tools;
    /// `hybrid`: meta-tools plus `pinned`.
    pub mode: Mode,
    /// Public tool names listed directly in hybrid mode.
    pub pinned: Vec<String>,
    /// Account per server (`github` → `work`) for servers whose secrets
    /// use `{account}`; others use `default`.
    pub accounts: std::collections::BTreeMap<String, String>,
    /// The servers this endpoint exposes (a profile's set); `None` = all.
    pub servers: Option<std::collections::BTreeSet<String>>,
    /// Tool policy for this endpoint (M2-08).
    pub policy: mcpgw_core::Policy,
    /// Name of this endpoint in messages (`profile frontend`).
    pub label: String,
    /// Where approvals for `approve`-listed tools are kept (M2-09). Without
    /// it such tools are refused.
    pub approvals: Option<Arc<mcpgw_runtime::Approvals>>,
    /// How long a call waits for `mcpgw approve` when the client can't be
    /// asked. Zero means the default of two minutes.
    pub approval_timeout: std::time::Duration,
    /// Where finished calls are recorded (M2-12).
    pub calls: Option<Arc<mcpgw_runtime::CallLog>>,
}

/// Routes front-side `tools/list` / `tools/call` to the backend manager.
pub struct GatewayProvider {
    manager: Arc<BackendManager>,
    opts: GatewayOptions,
}

impl GatewayProvider {
    /// Wrap a running backend manager, listing every tool directly.
    pub fn new(manager: Arc<BackendManager>) -> Self {
        Self::with_options(
            manager,
            GatewayOptions {
                mode: Mode::Direct,
                ..GatewayOptions::default()
            },
        )
    }

    /// Wrap a running backend manager with presentation options.
    pub fn with_options(manager: Arc<BackendManager>, opts: GatewayOptions) -> Self {
        Self { manager, opts }
    }

    fn scope(&self) -> Scope {
        Scope {
            servers: self.opts.servers.clone(),
            accounts: self.opts.accounts.clone(),
            policy: self.opts.policy.clone(),
            label: self.opts.label.clone(),
        }
    }

    /// Get a human's approval for this exact call. `Ok(())` lets it run;
    /// `Err` is the answer to give instead (a refusal, or the question).
    async fn approve(
        &self,
        tool: &str,
        args: &Option<JsonObject>,
        ctx: &front::CallContext,
    ) -> Result<(), Result<JsonObject, ProtoError>> {
        let refuse = |text: String| Err(Ok(meta_error(text)));
        let Some(ap) = &self.opts.approvals else {
            return refuse(format!(
                "Tool {tool} needs approval, but approvals are not available here."
            ));
        };
        // the client answered an earlier question (MRTR retry or elicitation)
        if let Some(input) = &ctx.input {
            let yes = input.response["action"] == "accept"
                && input.response["content"]["approve"] == true;
            let _ = ap.decide(&input.state, yes); // `mcpgw approve` may have decided first
            return match ap.redeem(&input.state, tool, args) {
                Ok(()) => Ok(()),
                Err(e) => refuse(format!("Tool {tool} was not run: {e}.")),
            };
        }
        let id = ap.request(tool, args, &self.opts.label, ctx.client_name.clone());
        let shown = args
            .as_ref()
            .map(|a| serde_json::Value::Object(a.clone()).to_string())
            .unwrap_or_else(|| "{}".into());
        let shown: String = shown.chars().take(500).collect();
        if ctx.can_ask() {
            return Err(Err(ProtoError::Elicit {
                state: id.clone(),
                message: format!(
                    "Allow {tool} on {}?\nArguments: {shown}\n(or run `mcpgw approve {id}`)",
                    self.opts.label
                ),
                schema: serde_json::json!({
                    "type": "object",
                    "properties": {"approve": {"type": "boolean", "title": "Approve this call", "default": false}},
                    "required": ["approve"]
                }),
            }));
        }
        // the client can't be asked: a person approves from the CLI
        let wait = if self.opts.approval_timeout.is_zero() {
            std::time::Duration::from_secs(120)
        } else {
            self.opts.approval_timeout
        };
        tracing::warn!(%id, tool, "approval needed: run `mcpgw approve {id}` or `mcpgw deny {id}`");
        match ap.wait(&id, wait).await {
            Some(true) => match ap.redeem(&id, tool, args) {
                Ok(()) => Ok(()),
                Err(e) => refuse(format!("Tool {tool} was not run: {e}.")),
            },
            Some(false) => {
                ap.cancel(&id);
                refuse(format!("Tool {tool} was not approved."))
            }
            None => {
                ap.cancel(&id);
                refuse(format!(
                    "Tool {tool} needs approval and none came within {}s. Ask the user to watch `mcpgw approvals` and approve it, then call it again.",
                    wait.as_secs()
                ))
            }
        }
    }

    /// Start the login a failed call needs and answer with its link.
    async fn login_answer(
        &self,
        e: ProtoError,
        public: &str,
        ctx: &front::CallContext,
        link: &login::LoginLink,
    ) -> Result<JsonObject, ProtoError> {
        let Some((server, _)) = resolve_in(&self.manager, public, &self.scope()).await else {
            return Err(e);
        };
        let account = self.opts.accounts.get(&server).map(String::as_str);
        let (provider, account) = login::login_target(&e, &server, account);
        match link(provider.clone(), account.clone()).await {
            Ok((id, url)) => {
                tracing::info!(%provider, %account, "call needs a login; sent a sign-in link");
                login::respond(ctx, &provider, &account, &id, &url)
            }
            Err(why) => {
                tracing::warn!(%provider, %account, error = %why, "could not start a login");
                Err(e)
            }
        }
    }

    async fn listing(&self) -> Vec<JsonObject> {
        match self.opts.mode {
            Mode::Direct => list_tools_in(&self.manager, &self.scope()).await,
            Mode::Meta => meta::meta_tools(),
            Mode::Hybrid => {
                let mut out = meta::meta_tools();
                out.extend(
                    list_tools_in(&self.manager, &self.scope())
                        .await
                        .into_iter()
                        .filter(|t| {
                            t["name"]
                                .as_str()
                                .is_some_and(|n| self.opts.pinned.iter().any(|p| p == n))
                        }),
                );
                out
            }
        }
    }
}

/// `ttlMs` of a direct or hybrid listing: short, so stateless clients
/// re-list soon after servers change.
pub const LIST_TTL_MS: u64 = 5_000;
/// `ttlMs` of the meta-tools listing, which never changes.
pub const META_LIST_TTL_MS: u64 = 3_600_000;

impl ToolProvider for GatewayProvider {
    fn changes(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        // the meta-tools listing never changes, so there is nothing to tell
        (self.opts.mode != Mode::Meta).then(|| self.manager.catalog().changes())
    }

    fn list_ttl_ms(&self) -> Option<u64> {
        Some(match self.opts.mode {
            Mode::Meta => META_LIST_TTL_MS,
            _ => LIST_TTL_MS,
        })
    }

    fn list_tools(&self) -> BoxFuture<'_, Vec<JsonObject>> {
        Box::pin(async move {
            let r: Result<_, std::convert::Infallible> =
                mcpgw_telemetry::instrument_request("tools/list", None, async {
                    Ok(self.listing().await)
                })
                .await;
            r.unwrap_or_else(|never| match never {})
        })
    }

    fn call_tool(
        &self,
        name: String,
        arguments: Option<JsonObject>,
        ctx: front::CallContext,
    ) -> BoxFuture<'_, Result<JsonObject, ProtoError>> {
        Box::pin(async move {
            // In meta and hybrid modes the meta-tools exist; any tool can still
            // be called directly by its public name in every mode.
            let meta = self.opts.mode != Mode::Direct && meta::is_meta(&name);
            let label = match (meta, &arguments) {
                (true, Some(a)) if name == meta::CALL => a
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(meta::CALL)
                    .to_string(),
                _ => name.clone(),
            };
            let started = std::time::Instant::now();
            let client = ctx.client_name.clone();
            let out = mcpgw_telemetry::instrument_request("tools/call", Some(&label), async {
                // the tool actually being run: the name, or call_tool's target
                let target = match (meta, name.as_str()) {
                    (false, _) => Some((name.clone(), arguments.clone())),
                    (true, meta::CALL) => arguments.as_ref().and_then(|a| {
                        let t = a.get("name")?.as_str()?.to_string();
                        Some((t, a.get("arguments").and_then(|v| v.as_object()).cloned()))
                    }),
                    _ => None,
                };
                if let Some((tool, targs)) = &target
                    && let mcpgw_core::Decision::Approve { .. } = self.opts.policy.decide(tool)
                    && let Err(answer) = self.approve(tool, targs, &ctx).await
                {
                    return answer;
                }
                let res = if meta {
                    meta::call(&self.manager, &name, arguments, &self.scope()).await
                } else {
                    call_in(&self.manager, &name, arguments, &self.scope()).await
                };
                match (res, &self.opts.login) {
                    (Err(e), Some(link)) if login::needs_login(&e) => {
                        self.login_answer(e, &label, &ctx, link).await
                    }
                    (res, _) => res,
                }
            })
            .await;
            // the question of an approval round isn't a finished call
            if let Some(log) = &self.opts.calls
                && !matches!(out, Err(ProtoError::Elicit { .. }))
                && !(meta && name != meta::CALL)
            {
                let outcome = match &out {
                    Ok(r) if r.get("isError").and_then(|v| v.as_bool()) == Some(true) => {
                        "tool_error"
                    }
                    Ok(_) => "ok",
                    Err(_) => "error",
                };
                log.record(
                    &self.opts.label,
                    &label,
                    outcome,
                    started.elapsed().as_millis() as u64,
                    client,
                );
            }
            out
        })
    }
}

fn meta_error(text: String) -> JsonObject {
    serde_json::json!({"content": [{"type": "text", "text": text}], "isError": true})
        .as_object()
        .cloned()
        .expect("object")
}

/// Default identity advertised to clients.
pub fn front_info() -> FrontInfo {
    FrontInfo {
        name: "mcpgw".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        instructions: Some(
            "Tools come from several MCP servers and are named `<server>__<tool>`. If only \
             search_tools, describe_tool and call_tool are listed, search for the tool you need \
             first, then call it with call_tool."
                .into(),
        ),
    }
}

/// Serve the gateway over any byte stream (tests, sockets).
pub async fn serve_io<T>(manager: Arc<BackendManager>, io: T) -> Result<(), ProtoError>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
{
    front::serve_io(Arc::new(GatewayProvider::new(manager)), front_info(), io).await
}

/// The gateway's Streamable HTTP endpoint (mount under `/mcp`).
pub fn http_router(manager: Arc<BackendManager>, opts: GatewayOptions) -> axum::Router {
    front::http_service(
        Arc::new(GatewayProvider::with_options(manager, opts)),
        front_info(),
    )
}

/// Serve the gateway on stdin/stdout until the client disconnects.
pub async fn serve_stdio(
    manager: Arc<BackendManager>,
    opts: GatewayOptions,
) -> Result<(), ProtoError> {
    front::serve_stdio(
        Arc::new(GatewayProvider::with_options(manager, opts)),
        front_info(),
    )
    .await
}
