mod common;

use common::*;
use serde_json::{Value, json};

/// One stateless MCP request (protocol 2026-07-28) to `url`.
fn mcp(url: &str, method: &str, name: Option<&str>, params: Value) -> (u16, Value) {
    let mut params = params;
    params["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "0"},
        "io.modelcontextprotocol/clientCapabilities": {},
    });
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mut req = reqwest::Client::new()
            .post(url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", "2026-07-28")
            .header("mcp-method", method)
            .json(&body);
        if let Some(n) = name {
            req = req.header("mcp-name", n);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    })
}

#[test]
fn the_endpoint_lists_the_tools() {
    let h = harness();
    let url = format!("{}/r/{TOKEN}/mcp", serve(h.gw.clone()));
    let (status, r) = mcp(&url, "tools/list", None, json!({}));
    assert_eq!(status, 200, "{r}");
    let names: Vec<_> = r["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    let want = [
        "get_issue",
        "read_doc",
        "comment",
        "transition",
        "create_linked",
    ];
    assert_eq!(names, want);
}

#[test]
fn a_tool_call_goes_through_the_endpoint_and_the_policy() {
    let h = harness();
    let url = format!("{}/r/{TOKEN}/mcp", serve(h.gw.clone()));
    let params = json!({"name": "get_issue", "arguments": {"key": "FAKE-1"}});
    let (status, r) = mcp(&url, "tools/call", Some("get_issue"), params);
    assert_eq!(status, 200, "{r}");
    assert_ne!(r["result"]["isError"], true, "{r}");
    let issue: Value =
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(issue["key"], "FAKE-1");
    let refused = json!({"name": "get_issue", "arguments": {"key": "FAKE-9"}});
    let (_, r) = mcp(&url, "tools/call", Some("get_issue"), refused);
    assert_eq!(r["result"]["isError"], true, "{r}");
    assert!(!r.to_string().contains(PLANTED));
    assert_eq!(
        h.decisions().iter().map(|d| d.0).collect::<Vec<_>>(),
        [true, false]
    );
}

#[test]
fn a_publish_call_from_the_sandbox_is_refused() {
    let h = harness();
    let url = format!("{}/r/{TOKEN}/mcp", serve(h.gw.clone()));
    let files = json!([{"path": "src/evil.rs", "content": "steal()"}]);
    let params = json!({"name": "publish", "arguments": {"title": "t", "summary": "s",
        "test_evidence": "e", "files": files}});
    let (status, r) = mcp(&url, "tools/call", Some("publish"), params);
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["result"]["isError"], true, "{r}");
    assert!(r.to_string().contains("unknown tool"), "{r}");
    assert!(h.github.minted().is_empty());
    assert!(h.github.prs().is_empty());
}

#[test]
fn an_unknown_token_gets_no_endpoint() {
    let h = harness();
    let url = format!("{}/r/{}/mcp", serve(h.gw.clone()), "e".repeat(64));
    let (status, _) = mcp(&url, "tools/list", None, json!({}));
    assert_eq!(status, 404);
    assert!(h.records().is_empty());
}
