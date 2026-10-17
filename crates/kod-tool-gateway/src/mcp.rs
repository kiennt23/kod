//! The MCP endpoint. One route per run token, `/r/{token}/mcp`, served by
//! rmcp's Streamable HTTP transport in stateless JSON mode.
use crate::Gateway;
use axum::{
    Router,
    body::Body,
    extract::{Path, Request, State},
    http::StatusCode,
    response::IntoResponse,
    routing::any,
};
use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    },
    service::RequestContext,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

/// The run token of the request, put there by the route after it checked
/// the token names a run.
#[derive(Clone)]
struct RunToken(String);

#[derive(Clone)]
struct Handler(Arc<Gateway>);

fn schema(props: Value, required: &[&str]) -> Arc<serde_json::Map<String, Value>> {
    let s = json!({"type": "object", "properties": props, "required": required});
    match s {
        Value::Object(m) => Arc::new(m),
        _ => unreachable!(),
    }
}

/// The tools the endpoint lists: everything a run may call. Publish is not
/// one; only the host publishes, with `Gateway::publish`.
pub fn tools() -> Vec<Tool> {
    let s = json!({"type": "string"});
    vec![
        Tool::new(
            "get_issue",
            "Read a ticket: the run's own ticket, or a rework request Kod linked to it.",
            schema(json!({"key": s}), &["key"]),
        ),
        Tool::new(
            "read_doc",
            "Read an approved document linked from the run's ticket, by its pinned ID.",
            schema(json!({"id": s}), &["id"]),
        ),
        Tool::new(
            "comment",
            "Comment on the run's ticket, or a rework request Kod linked to it.",
            schema(json!({"key": s, "text": s}), &["key", "text"]),
        ),
        Tool::new(
            "transition",
            "Move the run's own ticket from in_progress to blocked, or back.",
            schema(json!({"key": s, "state": s}), &["key", "state"]),
        ),
        Tool::new(
            "create_linked",
            "Open a question, amendment or replan request linked to the run's ticket.",
            schema(
                json!({"kind": s, "stage": s, "artifact": s, "version": s, "text": s,
                       "evidence": {"type": "array", "items": s}}),
                &["kind", "stage", "artifact", "version", "text", "evidence"],
            ),
        ),
    ]
}

impl ServerHandler for Handler {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tools()))
    }

    async fn call_tool(
        &self,
        req: CallToolRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let token = ctx
            .extensions
            .get::<axum::http::request::Parts>()
            .and_then(|p| p.extensions.get::<RunToken>())
            .map(|t| t.0.clone())
            .ok_or_else(|| ErrorData::invalid_request("no run token", None))?;
        let args = Value::Object(req.arguments.unwrap_or_default());
        let (gateway, name) = (self.0.clone(), req.name.to_string());
        // The adapters block; keep them off the async workers.
        let reply = tokio::task::spawn_blocking(move || gateway.call(&token, &name, &args))
            .await
            .unwrap_or_else(|e| Err(format!("the tool call failed: {e}")));
        Ok(match reply {
            Ok(v) => CallToolResult::success(vec![ContentBlock::text(v.to_string())]),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(e)]),
        }
        .into())
    }
}

/// The routes of the tool gateway. `hosts` are the `Host` values the
/// endpoint answers to, as rmcp checks them against DNS rebinding.
pub fn router(gateway: Arc<Gateway>, hosts: Vec<String>) -> Router {
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_sse_keep_alive(None)
        .with_allowed_hosts(hosts);
    let handler = Handler(gateway.clone());
    let service = StreamableHttpService::new(
        move || Ok(handler.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    Router::new()
        .route(
            "/r/{token}/mcp",
            any(
                move |State(g): State<Arc<Gateway>>,
                      Path(token): Path<String>,
                      mut req: Request| {
                    let service = service.clone();
                    async move {
                        if g.run(&token).is_none() {
                            return (StatusCode::NOT_FOUND, "unknown run").into_response();
                        }
                        req.extensions_mut().insert(RunToken(token));
                        match service.oneshot(req).await {
                            Ok(r) => r.map(Body::new),
                            Err(e) => match e {},
                        }
                    }
                },
            ),
        )
        .with_state(gateway)
}
