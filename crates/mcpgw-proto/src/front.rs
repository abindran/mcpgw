//! The client-facing MCP server.
//!
//! Other crates implement [`ToolProvider`] with plain JSON; this module adapts
//! it to an `rmcp` server that answers both `server/discover` (2026-07-28)
//! and legacy `initialize` (2025-11-25) clients, over stdio or Streamable
//! HTTP ([`http_service`]).

use std::{future::Future, pin::Pin, sync::Arc};

use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ErrorCode, Implementation,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
    },
    service::RequestContext,
};
use serde_json::Value;

use crate::backend::{INTERNAL_ERROR, JsonObject, ProtoError, tool_from_json};

/// Boxed, sendable future.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What the front server exposes. Implemented outside the protocol crate.
pub trait ToolProvider: Send + Sync + 'static {
    /// Every tool, as MCP `Tool` JSON objects.
    fn list_tools(&self) -> BoxFuture<'_, Vec<JsonObject>>;
    /// Execute a tool; `Ok` is a `CallToolResult` JSON object.
    fn call_tool(
        &self,
        name: String,
        arguments: Option<JsonObject>,
        ctx: CallContext,
    ) -> BoxFuture<'_, Result<JsonObject, ProtoError>>;

    /// A counter that goes up when the tool list may have changed. With it,
    /// clients are told (`notifications/tools/list_changed`) instead of
    /// having to reconnect (M2-10).
    fn changes(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        None
    }

    /// How long clients may cache `tools/list` (`ttlMs`, 2026-07-28).
    fn list_ttl_ms(&self) -> Option<u64> {
        None
    }

    /// Which endpoint this is, for tasks (M4-09): a task is only visible
    /// to the endpoint that created it.
    fn task_scope(&self) -> String {
        String::new()
    }
}

/// How one attempt at a call ended.
enum Outcome {
    /// The `CallToolResult`.
    Done(JsonObject),
    /// Still running: it continues as this task.
    Task(rmcp::model::Task),
}

/// Aborts a spawned call when the request that started it goes away.
struct AbortOnDrop(Option<tokio::task::AbortHandle>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(a) = self.0.take() {
            a.abort();
        }
    }
}

/// The JSON-RPC error object for a failed task.
fn error_object(e: ProtoError) -> JsonObject {
    match serde_json::to_value(to_error_data(e)) {
        Ok(Value::Object(o)) => o,
        _ => JsonObject::new(),
    }
}

/// Feed a call that became a task into its task: progress becomes the
/// status message, the outcome its result or error.
async fn follow(
    id: String,
    mut job: tokio::task::JoinHandle<Result<JsonObject, ProtoError>>,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<crate::progress::Progress>,
) {
    let tasks = crate::tasks::Tasks::global();
    loop {
        tokio::select! {
            r = &mut job => {
                match r {
                    Ok(Ok(result)) => tasks.complete(&id, result),
                    Ok(Err(e)) => tasks.fail(&id, error_object(e)),
                    Err(_) => {} // aborted by tasks/cancel, already recorded
                }
                return;
            }
            Some(p) = rx.recv() => tasks.progress(&id, &p),
        }
    }
}

/// What the gateway knows about the client making a call.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CallContext {
    /// The client handles URL-mode elicitation (MCP 2025-11-25), so a
    /// `-32042` "URL elicitation required" error will show it a link.
    pub url_elicitation: bool,
    /// The client speaks 2026-07-28 and handles form elicitation, so input
    /// is requested with MRTR (`InputRequiredResult`) and comes back on the
    /// retried call.
    pub mrtr: bool,
    /// A legacy client that handles form elicitation on its session.
    pub form_elicitation: bool,
    /// The client's name, if it said.
    pub client_name: Option<String>,
    /// The answer to an earlier [`ProtoError::Elicit`].
    pub input: Option<Input>,
    /// The client's W3C trace context (`_meta.traceparent`, else the
    /// `traceparent` HTTP header), to continue its trace (M3-11).
    pub traceparent: Option<String>,
    /// Its `tracestate`, likewise.
    pub tracestate: Option<String>,
}

impl CallContext {
    /// Whether the front end can ask this client a question.
    pub fn can_ask(&self) -> bool {
        self.mrtr || self.form_elicitation
    }
}

/// A client's answer to [`ProtoError::Elicit`].
#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    /// The `state` of the question.
    pub state: String,
    /// The elicitation result: `{"action": "accept", "content": {…}}`.
    pub response: Value,
}

/// Key of the approval question in MRTR input requests and responses.
const INPUT_KEY: &str = "approval";

/// How long a legacy client gets to answer an elicitation.
const ELICIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// JSON-RPC error code: the request needs the user to visit a URL first.
pub const URL_ELICITATION_REQUIRED: i32 = -32042;

/// Identity + instructions advertised to clients.
#[derive(Debug, Clone)]
pub struct FrontInfo {
    /// Implementation name.
    pub name: String,
    /// Implementation version.
    pub version: String,
    /// Optional instructions for the model.
    pub instructions: Option<String>,
}

struct Front<P> {
    provider: Arc<P>,
    info: FrontInfo,
}

impl<P> Clone for Front<P> {
    fn clone(&self) -> Self {
        Self {
            provider: self.provider.clone(),
            info: self.info.clone(),
        }
    }
}

/// Convert a seam error into a JSON-RPC error for the front client.
pub fn to_error_data(e: ProtoError) -> ErrorData {
    match e {
        ProtoError::Rpc {
            code,
            message,
            data,
        } => ErrorData::new(ErrorCode(code), message, data),
        other => ErrorData::new(ErrorCode(INTERNAL_ERROR), other.to_string(), None),
    }
}

impl<P: ToolProvider> Front<P> {
    /// Run one attempt at a call: progress goes to the client while it
    /// waits, a cancelled request stops the call, and for a client that
    /// declared tasks a call still running after [`crate::tasks::TASK_AFTER`]
    /// continues as a task (M4-09).
    async fn run(
        &self,
        name: String,
        arguments: Option<JsonObject>,
        ctx: CallContext,
        context: &RequestContext<RoleServer>,
        token: Option<rmcp::model::ProgressToken>,
        tasks_ok: bool,
    ) -> Result<Outcome, ProtoError> {
        let (sink, mut rx) = crate::progress::ProgressSink::channel();
        let provider = self.provider.clone();
        let mut job = tokio::spawn(crate::progress::with_progress(sink, async move {
            provider.call_tool(name, arguments, ctx).await
        }));
        let mut guard = AbortOnDrop(Some(job.abort_handle()));
        let become_task = tokio::time::sleep(crate::tasks::TASK_AFTER);
        tokio::pin!(become_task);
        loop {
            tokio::select! {
                r = &mut job => {
                    guard.0 = None;
                    // progress sent before the end still reaches the client
                    while let Ok(p) = rx.try_recv() {
                        notify(context, &token, p).await;
                    }
                    let out = r.map_err(|e| ProtoError::Other(e.to_string()))??;
                    return Ok(Outcome::Done(out));
                }
                Some(p) = rx.recv() => notify(context, &token, p).await,
                _ = context.ct.cancelled() => {
                    return Err(ProtoError::Other("cancelled by the client".into()));
                }
                _ = &mut become_task, if tasks_ok => {
                    let abort = guard.0.take().expect("the call's abort handle");
                    let task = crate::tasks::Tasks::global()
                        .create(&self.provider.task_scope(), abort);
                    tokio::spawn(follow(task.task_id.clone(), job, rx));
                    return Ok(Outcome::Task(task));
                }
            }
        }
    }
}

/// Pass a call's progress to the client, under the client's token.
async fn notify(
    context: &RequestContext<RoleServer>,
    token: &Option<rmcp::model::ProgressToken>,
    p: crate::progress::Progress,
) {
    let Some(t) = token else {
        return;
    };
    let mut n = rmcp::model::ProgressNotificationParam::new(t.clone(), p.progress);
    n.total = p.total;
    n.message = p.message;
    let _ = context.peer.notify_progress(n).await;
}

impl<P: ToolProvider> ServerHandler for Front<P> {
    fn get_info(&self) -> ServerConfig {
        let caps = if self.provider.changes().is_some() {
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .enable_tasks()
                .build()
        } else {
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tasks()
                .build()
        };
        let mut cfg = ServerConfig::new(caps).with_server_info(Implementation::new(
            self.info.name.clone(),
            self.info.version.clone(),
        ));
        if let Some(i) = &self.info.instructions {
            cfg = cfg.with_instructions(i.clone());
        }
        cfg
    }

    // `ListToolsResult` is `#[non_exhaustive]`.
    #[allow(clippy::field_reassign_with_default)]
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let mut tools = Vec::new();
        for t in self.provider.list_tools().await {
            match tool_from_json(t) {
                Ok(t) => tools.push(t),
                Err(e) => tracing::warn!(error = %e, "skipping malformed tool"),
            }
        }
        let mut result = ListToolsResult::default();
        result.tools = tools;
        result.ttl_ms = self.provider.list_ttl_ms();
        Ok(result)
    }

    /// 2025-11-25 sessions: push `tools/list_changed` on the session.
    async fn on_initialized(&self, context: rmcp::service::NotificationContext<RoleServer>) {
        let Some(mut rx) = self.provider.changes() else {
            return;
        };
        rx.mark_unchanged();
        let peer = context.peer;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    r = rx.changed() => {
                        if r.is_err() || peer.notify_tool_list_changed().await.is_err() {
                            break;
                        }
                    }
                    // stop once the session is gone, even if nothing changes
                    _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
                        if peer.is_transport_closed() {
                            break;
                        }
                    }
                }
            }
        });
    }

    /// 2026-07-28: `subscriptions/listen` for `tools/list_changed`.
    fn accepted_subscription_filter(
        &self,
        _requested: &rmcp::model::SubscriptionFilter,
    ) -> Option<rmcp::model::SubscriptionFilter> {
        self.provider.changes().map(|_| {
            rmcp::model::SubscriptionFilter::builder()
                .tools_list_changed()
                .build()
        })
    }

    async fn listen(
        &self,
        subscription: rmcp::service::SubscriptionContext,
    ) -> Result<(), ErrorData> {
        let Some(mut rx) = self.provider.changes() else {
            subscription.cancelled().await;
            return Ok(());
        };
        rx.mark_unchanged();
        loop {
            tokio::select! {
                _ = subscription.cancelled() => return Ok(()),
                r = rx.changed() => {
                    if r.is_err() || subscription.sink().notify_tool_list_changed().await.is_err() {
                        return Ok(());
                    }
                }
            }
        }
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let caps = context.client_capabilities();
        let elicitation = caps.as_ref().and_then(|c| c.elicitation.as_ref());
        let modern = context
            .protocol_version()
            .is_some_and(|v| v >= rmcp::model::ProtocolVersion::V_2026_07_28);
        // an empty `elicitation: {}` means form (2025-06-18)
        let form = elicitation.is_some_and(|e| e.form.is_some() || e.url.is_none());
        // only ask what the client can answer: MRTR carries an elicitation too
        let mrtr = modern && form;
        // an MRTR retry carries the answer and our state
        let input = match (&request.request_state, &request.input_responses) {
            (Some(state), Some(r)) => r.get(INPUT_KEY).map(|response| Input {
                state: state.clone(),
                response: response.clone(),
            }),
            _ => None,
        };
        let trace = |k: &str| {
            request
                .meta
                .as_ref()
                .and_then(|m| m.get(k))
                .or_else(|| context.meta.get(k))
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| {
                    context
                        .extensions
                        .get::<axum::http::request::Parts>()
                        .and_then(|p| p.headers.get(k))
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string)
                })
        };
        let token = request
            .meta
            .as_ref()
            .and_then(|m| m.get_progress_token())
            .or_else(|| context.meta.get_progress_token());
        let tasks_ok = caps.as_ref().is_some_and(|c| c.supports_tasks());
        let mut ctx = CallContext {
            url_elicitation: elicitation.is_some_and(|e| e.url.is_some()),
            mrtr,
            form_elicitation: !modern && form,
            client_name: context.client_info().map(|i| i.name),
            input,
            traceparent: trace("traceparent"),
            tracestate: trace("tracestate"),
        };
        let out = loop {
            let res = self
                .run(
                    request.name.to_string(),
                    request.arguments.clone(),
                    ctx.clone(),
                    &context,
                    token.clone(),
                    tasks_ok,
                )
                .await;
            match res {
                Err(ProtoError::Elicit {
                    state,
                    message,
                    schema,
                }) if ctx.mrtr => {
                    let ask: rmcp::model::InputRequest = serde_json::from_value(serde_json::json!({
                        "method": "elicitation/create",
                        "params": {"mode": "form", "message": message, "requestedSchema": schema},
                    }))
                    .map_err(|e| ErrorData::new(ErrorCode(INTERNAL_ERROR), e.to_string(), None))?;
                    let requests = [(INPUT_KEY.to_string(), ask)].into_iter().collect();
                    return Ok(
                        rmcp::model::InputRequiredResult::new(Some(requests), Some(state)).into(),
                    );
                }
                Err(ProtoError::Elicit {
                    state,
                    message,
                    schema,
                }) if ctx.form_elicitation && ctx.input.is_none() => {
                    let params: rmcp::model::ElicitRequestParams =
                        serde_json::from_value(serde_json::json!({
                            "mode": "form", "message": message, "requestedSchema": schema,
                        }))
                        .map_err(|e| {
                            ErrorData::new(ErrorCode(INTERNAL_ERROR), e.to_string(), None)
                        })?;
                    let response = match context
                        .peer
                        .create_elicitation_with_timeout(params, Some(ELICIT_TIMEOUT))
                        .await
                    {
                        Ok(r) => serde_json::to_value(r).unwrap_or_default(),
                        Err(e) => {
                            tracing::warn!(error = %e, "elicitation failed");
                            serde_json::json!({"action": "cancel"})
                        }
                    };
                    ctx.input = Some(Input { state, response });
                }
                Err(e) => return Err(to_error_data(e)),
                Ok(Outcome::Task(task)) => {
                    return Ok(rmcp::model::CreateTaskResult::new(task).into());
                }
                Ok(Outcome::Done(out)) => break out,
            }
        };
        let result: CallToolResult = serde_json::from_value(Value::Object(out)).map_err(|e| {
            ErrorData::new(
                ErrorCode(INTERNAL_ERROR),
                format!("backend returned malformed result: {e}"),
                None,
            )
        })?;
        Ok(result.into())
    }

    /// SEP-2663 `tasks/get` (M4-09).
    async fn get_task(
        &self,
        request: rmcp::model::GetTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::GetTaskResult, ErrorData> {
        crate::tasks::Tasks::global()
            .get(&self.provider.task_scope(), &request.task_id)
            .map(rmcp::model::GetTaskResult::new)
            .map_err(|e| ErrorData::invalid_params(format!("no task {}", e.0), None))
    }

    /// SEP-2663 `tasks/cancel`: stops the call, at the backend too.
    async fn cancel_task(
        &self,
        request: rmcp::model::CancelTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        crate::tasks::Tasks::global()
            .cancel(&self.provider.task_scope(), &request.task_id)
            .map_err(|e| ErrorData::invalid_params(format!("no task {}", e.0), None))
    }

    /// SEP-2663 `tasks/update`: gateway tasks never wait for input (approvals
    /// are asked before a call starts).
    async fn update_task(
        &self,
        request: rmcp::model::UpdateTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        Err(ErrorData::invalid_params(
            format!("task {} has no input requests", request.task_id),
            None,
        ))
    }
}

/// Serve `provider` over an arbitrary byte stream until the client hangs up.
pub async fn serve_io<P, T>(provider: Arc<P>, info: FrontInfo, io: T) -> Result<(), ProtoError>
where
    P: ToolProvider,
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
{
    let svc = Front { provider, info }
        .serve(io)
        .await
        .map_err(|e| ProtoError::Connect(e.to_string()))?;
    svc.waiting()
        .await
        .map_err(|e| ProtoError::Other(e.to_string()))?;
    Ok(())
}

/// Serve `provider` on this process's stdin/stdout.
pub async fn serve_stdio<P: ToolProvider>(
    provider: Arc<P>,
    info: FrontInfo,
) -> Result<(), ProtoError> {
    serve_io(
        provider,
        info,
        tokio::io::join(tokio::io::stdin(), tokio::io::stdout()),
    )
    .await
}

/// The Streamable HTTP endpoint as an `axum` router (mount it with
/// `Router::nest_service`). 2026-07-28 requests are stateless; legacy clients
/// get sessions. Authentication, `Host` and `Origin` checks belong to the
/// caller's middleware; rmcp's own loopback-only `Host` check stays on as a
/// second line of defence.
pub fn http_service<P: ToolProvider>(provider: Arc<P>, info: FrontInfo) -> axum::Router {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };
    let front = Front { provider, info };
    let service = StreamableHttpService::new(
        move || Ok(front.clone()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    axum::Router::new().fallback_service(service)
}
