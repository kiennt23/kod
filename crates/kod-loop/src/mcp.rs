//! A client for the tool gateway's MCP endpoint: stateless Streamable HTTP
//! (protocol 2026-07-28), JSON responses, one POST per call.
use crate::tools::{self, ToolResult};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::time::Duration;

pub const PROTOCOL: &str = "2026-07-28";
/// One gateway call; a publish of a large change is the slowest.
pub const TIMEOUT: Duration = Duration::from_secs(120);

pub struct Mcp {
    url: String,
    http: reqwest::blocking::Client,
    /// The offered tools, in the Messages API's tool format.
    definitions: Vec<Value>,
}

impl Mcp {
    /// Lists the gateway's tools. Fails if the endpoint can't be reached or
    /// a tool would shadow a local one.
    pub fn connect(url: &str) -> Result<Mcp> {
        let http = reqwest::blocking::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .context("building the MCP client")?;
        let mut mcp = Mcp {
            url: url.to_owned(),
            http,
            definitions: vec![],
        };
        let listed = mcp.rpc("tools/list", None, json!({}))?;
        let local = tools::definitions();
        let local: Vec<&str> = local
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t["name"].as_str())
            .collect();
        for t in listed["tools"].as_array().context("tools/list: no tools")? {
            let name = t["name"]
                .as_str()
                .context("tools/list: a tool has no name")?;
            if local.contains(&name) {
                bail!("the gateway's tool {name:?} would shadow a local tool");
            }
            mcp.definitions.push(json!({
                "name": name,
                "description": t["description"],
                "input_schema": t["inputSchema"],
            }));
        }
        Ok(mcp)
    }

    pub fn definitions(&self) -> &[Value] {
        &self.definitions
    }

    pub fn offers(&self, name: &str) -> bool {
        self.definitions.iter().any(|d| d["name"] == name)
    }

    /// Calls a tool. Transport and protocol errors come back as an error
    /// result the model can see; the gateway already refused or acted.
    pub fn call(&self, name: &str, input: &Value) -> ToolResult {
        let params = json!({"name": name, "arguments": input});
        let (text, is_error) = match self.rpc("tools/call", Some(name), params) {
            Ok(r) => {
                let text = r["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|c| c["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                (text, r["isError"] == true)
            }
            Err(e) => (scrub(&format!("the tool gateway failed: {e:#}")), true),
        };
        ToolResult {
            text,
            is_error,
            finish: None,
        }
    }

    fn rpc(&self, method: &str, name: Option<&str>, mut params: Value) -> Result<Value> {
        params["_meta"] = json!({
            "io.modelcontextprotocol/protocolVersion": PROTOCOL,
            "io.modelcontextprotocol/clientInfo": {"name": "kod-loop", "version": env!("CARGO_PKG_VERSION")},
            "io.modelcontextprotocol/clientCapabilities": {},
        });
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let mut req = self
            .http
            .post(&self.url)
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", PROTOCOL)
            .header("mcp-method", method)
            .header("content-type", "application/json")
            .body(body.to_string());
        if let Some(n) = name {
            req = req.header("mcp-name", n);
        }
        // reqwest puts the URL, and so the run token, in its errors.
        let resp = req
            .send()
            .map_err(|e| anyhow!("reaching the tool gateway: {}", e.without_url()))?;
        let status = resp.status();
        let text = resp
            .text()
            .map_err(|e| anyhow!("reading the tool gateway's reply: {}", e.without_url()))?;
        if !status.is_success() {
            let scrubbed = scrub(&text);
            let head = crate::api::head(&scrubbed, 200);
            bail!("{method}: HTTP {status}: {head}");
        }
        let mut r: Value = serde_json::from_str(&text)
            .with_context(|| format!("{method}: the reply is not JSON"))?;
        if !r["error"].is_null() {
            bail!("{method}: {}", r["error"]);
        }
        match r.get_mut("result").map(Value::take) {
            Some(result) if result.is_object() => Ok(result),
            _ => bail!("{method}: the reply has no result"),
        }
    }
}

/// Replaces a run token in a `/r/<token>/` path with `[REDACTED]`: the
/// token is the run's credential and must not reach the trace.
pub fn scrub(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("/r/") {
        out.push_str(&rest[..i + 3]);
        rest = &rest[i + 3..];
        let hex = rest
            .bytes()
            .take_while(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
            .count();
        if hex >= 32 {
            out.push_str("[REDACTED]");
            rest = &rest[hex..];
        }
    }
    out.push_str(rest);
    out
}
