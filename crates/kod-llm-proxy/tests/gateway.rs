//! Routing, budgets and audit capture: the parts phase 1 adds.
mod common;

use axum::{Router, body::Bytes, routing::post};
use common::{
    CLASS, Gateway, MODEL, Setup, budget, entries, gateway, limits, msg, post_msg, run, serve,
};
use kod_llm_proxy::{
    CAPTURE_MAX, Limits, MAX_IMAGES,
    budget::{IMAGE_TOKENS, reservation},
    capture,
    config::Budget,
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

/// A body that reports `usage` and echoes the request, plus a call counter.
async fn upstream(usage: &'static str) -> (String, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let router = Router::new().route(
        "/v1/messages",
        post(move |body: Bytes| {
            seen.fetch_add(1, Ordering::SeqCst);
            async move {
                format!(
                    r#"{{"usage":{usage},"echo":{:?}}}"#,
                    String::from_utf8_lossy(&body)
                )
            }
        }),
    );
    (serve(router).await, calls)
}

const SMALL: &str = r#"{"input_tokens":10,"output_tokens":10}"#;

async fn status(g: &Gateway, body: String) -> u16 {
    post_msg(&g.url, body).await.status().as_u16()
}

#[tokio::test]
async fn an_unknown_run_is_refused_without_a_call() {
    let (up, calls) = upstream(SMALL).await;
    let g = gateway(&up, Setup::default()).await;
    let root = g.url.split("/r/").next().unwrap().to_owned();
    for url in [
        root.clone(),
        format!("{root}/r/{}", "f".repeat(32)),
        format!("{root}/r/"),
    ] {
        assert_eq!(post_msg(&url, msg(MODEL, 10)).await.status(), 401, "{url}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(entries(&g.db).is_empty());
}

#[tokio::test]
async fn an_unknown_data_class_is_refused_and_recorded() {
    let (up, calls) = upstream(SMALL).await;
    for classes in [&["customer_pii"][..], &[CLASS, "customer_pii"], &[]] {
        let g = gateway(
            &up,
            Setup {
                run: run(classes, budget()),
                ..Setup::default()
            },
        )
        .await;
        assert_eq!(status(&g, msg(MODEL, 10)).await, 403, "{classes:?}");
        let e = entries(&g.db);
        assert_eq!(e.len(), 2, "{e:?}");
        assert_eq!(e[0].0, "run_audit");
        assert_eq!(e[0].1["decision"]["allow"], false);
        assert_eq!(e[1].0, "run_audit");
        assert_eq!(e[1].1["kind"], "gateway_refusal");
        assert_eq!(e[1].1["event"], "deny_route");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_disallowed_endpoint_is_refused() {
    let (up, calls) = upstream(SMALL).await;
    let g = gateway(
        &up,
        Setup {
            endpoint: "anthropic",
            ..Setup::default()
        },
    )
    .await;
    assert_eq!(status(&g, msg(MODEL, 10)).await, 403);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let e = entries(&g.db);
    let reasons = e[0].1["decision"]["reasons"].to_string();
    assert!(
        reasons.contains("endpoint anthropic is not allowed"),
        "{reasons}"
    );
}

#[tokio::test]
async fn spend_stops_at_the_budget() {
    let (up, calls) = upstream(SMALL).await;
    let body = msg(MODEL, 10);
    let r = reservation(&common::PRICE, body.len(), 0, 10);
    // Room for one reservation plus 30 settled tokens: each call settles at 20.
    let g = gateway(
        &up,
        Setup {
            run: run(
                &[CLASS],
                Budget {
                    max_tokens: r.tokens + 30,
                    max_cost_micro_usd: 1_000_000,
                },
            ),
            ..Setup::default()
        },
    )
    .await;
    assert_eq!(status(&g, body.clone()).await, 200);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(status(&g, body.clone()).await, 200);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(status(&g, body.clone()).await, 403);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn the_cost_limit_is_enforced_too() {
    let (up, calls) = upstream(SMALL).await;
    // A zero cost cap (runs.json refuses it, a hand-built run can have it)
    // is below any reservation, however many tokens are left.
    let g = gateway(
        &up,
        Setup {
            run: run(
                &[CLASS],
                Budget {
                    max_tokens: 1_000_000_000,
                    max_cost_micro_usd: 0,
                },
            ),
            ..Setup::default()
        },
    )
    .await;
    assert_eq!(status(&g, msg(MODEL, 10)).await, 403);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn concurrent_requests_cannot_overspend_together() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let slow = Router::new().route(
        "/v1/messages",
        post(move || {
            seen.fetch_add(1, Ordering::SeqCst);
            async {
                tokio::time::sleep(Duration::from_millis(300)).await;
                r#"{"usage":{"input_tokens":1,"output_tokens":1}}"#
            }
        }),
    );
    let up = serve(slow).await;
    let body = msg(MODEL, 10);
    let r = reservation(&common::PRICE, body.len(), 0, 10);
    let g = gateway(
        &up,
        Setup {
            run: run(
                &[CLASS],
                Budget {
                    max_tokens: r.tokens * 3 / 2,
                    max_cost_micro_usd: 1_000_000,
                },
            ),
            ..Setup::default()
        },
    )
    .await;
    let calls_out: Vec<_> = (0..2)
        .map(|_| {
            let (url, body) = (g.url.clone(), body.clone());
            tokio::spawn(async move { post_msg(&url, body).await.status().as_u16() })
        })
        .collect();
    let mut got = vec![];
    for c in calls_out {
        got.push(c.await.unwrap());
    }
    got.sort();
    assert_eq!(got, [200, 403]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_response_costing_more_than_its_reservation_halts_the_run() {
    let (up, calls) = upstream(r#"{"input_tokens":10,"output_tokens":1000000}"#).await;
    let g = gateway(&up, Setup::default()).await;
    assert_eq!(status(&g, msg(MODEL, 10)).await, 200);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(status(&g, msg(MODEL, 10)).await, 403);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let e = entries(&g.db);
    assert!(
        e.iter().any(|(t, p)| t == "run_audit"
            && p["kind"] == "gateway_halt"
            && p["reason"] == "a response cost more than its reservation"),
        "{e:?}"
    );
}

#[tokio::test]
async fn a_queued_request_is_refused_after_the_run_halts() {
    queued_request_is_refused(false).await;
    queued_request_is_refused(true).await;
}

async fn queued_request_is_refused(audit_failure: bool) {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let finish = Arc::new(tokio::sync::Notify::new());
    let release = finish.clone();
    let up = serve(Router::new().route(
        "/v1/messages",
        post(move || {
            let release = release.clone();
            let first = seen.fetch_add(1, Ordering::SeqCst) == 0;
            async move {
                if first {
                    release.notified().await;
                }
                let usage = if audit_failure {
                    SMALL
                } else {
                    r#"{"input_tokens":10,"output_tokens":1000000}"#
                };
                format!(r#"{{"usage":{usage}}}"#)
            }
        }),
    ))
    .await;
    let g = gateway(
        &up,
        Setup {
            limits: Limits {
                max_concurrent: 1,
                ..limits()
            },
            ..Setup::default()
        },
    )
    .await;
    let mut tasks = vec![];
    for _ in 0..2 {
        let url = g.url.clone();
        tasks.push(tokio::spawn(async move {
            let response = post_msg(&url, msg(MODEL, 10)).await;
            let status = response.status().as_u16();
            response.bytes().await.unwrap();
            status
        }));
    }
    // Both requests have reserved and written their prompts; one is queued.
    tokio::time::timeout(Duration::from_secs(5), async {
        while entries(&g.db)
            .iter()
            .filter(|(t, _)| t == "prompts")
            .count()
            < 2
            || calls.load(Ordering::SeqCst) == 0
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    if audit_failure {
        std::fs::remove_file(&g.db).unwrap();
    }
    finish.notify_one();
    let mut statuses = vec![];
    for task in tasks {
        statuses.push(
            tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .unwrap()
                .unwrap(),
        );
    }
    statuses.sort();
    assert_eq!(statuses, [200, 403]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn prompts_and_outputs_are_captured_redacted() {
    let (up, _) = upstream(SMALL).await;
    let g = gateway(&up, Setup::default()).await;
    let secret = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789";
    let body = format!(
        r#"{{"model":"{MODEL}","max_tokens":10,"messages":[{{"role":"user","content":"key {secret}"}}]}}"#
    );
    let res = post_msg(&g.url, body).await;
    assert_eq!(res.status(), 200);
    // The client still gets the response unchanged.
    assert!(res.text().await.unwrap().contains(secret));
    tokio::time::sleep(Duration::from_millis(200)).await;
    let e = entries(&g.db);
    let types: Vec<_> = e.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(types, ["run_audit", "prompts", "model_outputs"]);
    assert_eq!(e[1].1["run"], "r1");
    assert_eq!(e[1].1["model"], MODEL);
    assert_eq!(e[2].1["usage"]["output_tokens"], 10);
    for (_, payload) in &e {
        assert!(!payload.to_string().contains(secret), "{payload}");
    }
    assert!(e[1].1["request"]["text"].as_str().unwrap().contains("key "));
}

#[tokio::test]
async fn a_long_body_is_captured_cut_with_its_length_and_hash() {
    let text = "a".repeat(CAPTURE_MAX + 10);
    let c = capture(&text, true, text.len() as u64, "h".into());
    assert_eq!(c["text"].as_str().unwrap().len(), CAPTURE_MAX);
    assert_eq!(c["bytes"], CAPTURE_MAX + 10);
    assert_eq!(c["truncated"], true);
    let c = capture("short", false, 5, "h".into());
    assert_eq!(c["truncated"], true);
}

#[test]
fn a_secret_on_the_capture_cut_is_redacted_not_cut() {
    let secret = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789";
    let text = format!("{} {secret}", "a".repeat(CAPTURE_MAX - 21));
    let c = capture(&text, true, text.len() as u64, "h".into());
    let kept = c["text"].as_str().unwrap();
    assert!(!kept.contains("sk-ant"), "{}", &kept[kept.len() - 40..]);
    assert!(kept.ends_with("[REDACTED]"));
}

/// An upstream that streams `pieces` as text deltas of one content block.
async fn streaming(pieces: &'static [&'static str]) -> String {
    let router = Router::new().route(
        "/v1/messages",
        post(move || async move {
            let mut events = vec![
                json!({"type": "message_start",
                    "message": {"usage": {"input_tokens": 5, "output_tokens": 1}}}),
                json!({"type": "content_block_start", "index": 0,
                    "content_block": {"type": "text", "text": ""}}),
            ];
            for p in pieces {
                events.push(json!({"type": "content_block_delta", "index": 0,
                    "delta": {"type": "text_delta", "text": p}}));
            }
            events.push(json!({"type": "message_delta", "usage": {"output_tokens": 9}}));
            events.push(json!({"type": "message_stop"}));
            let body: String = events
                .iter()
                .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
                .collect();
            ([("content-type", "text/event-stream")], body)
        }),
    );
    serve(router).await
}

#[tokio::test]
async fn a_secret_split_across_stream_deltas_is_redacted() {
    let up = streaming(&[
        "the key is sk-ant-api0",
        "3-abcdefghijklmnop",
        "qrstuvwxyz0123456789 ok",
    ])
    .await;
    let g = gateway(&up, Setup::default()).await;
    let res = post_msg(&g.url, msg(MODEL, 10)).await;
    assert_eq!(res.status(), 200);
    res.text().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let e = entries(&g.db);
    let out = &e.iter().find(|(t, _)| t == "model_outputs").unwrap().1;
    assert_eq!(out["response"]["text"], "the key is [REDACTED] ok", "{out}");
    for piece in ["sk-ant-api0", "abcdefghijklmnop", "qrstuvwxyz"] {
        assert!(!out.to_string().contains(piece), "{out}");
    }
}

/// `msg` with extra top-level fields and one user message of `content`.
fn body_with(extra: &str, content: Value) -> String {
    format!(
        r#"{{"model":"{MODEL}","max_tokens":10{extra},"messages":[{{"role":"user","content":{content}}}]}}"#
    )
}

fn image(source: Value) -> Value {
    json!({"type": "image", "source": source})
}

#[tokio::test]
async fn refuses_every_body_shape_it_cannot_price_or_see() {
    let (up, calls) = upstream(SMALL).await;
    // Room for MAX_IMAGES + 1 small images.
    let limits = Limits {
        max_body: 64 << 10,
        ..limits()
    };
    let g = gateway(
        &up,
        Setup {
            limits,
            ..Setup::default()
        },
    )
    .await;
    let b64 = json!({"type": "base64", "media_type": "image/png", "data": "iVBO"});
    let url = json!({"type": "url", "url": "https://attacker.example/x.png"});
    let file = json!({"type": "file", "file_id": "file_1"});
    let cases = [
        (
            "an unknown field",
            body_with(r#","service_tier":"priority""#, json!("hi")),
        ),
        (
            "mcp_servers",
            body_with(
                r#","mcp_servers":[{"type":"url","url":"https://attacker.example","name":"x"}]"#,
                json!("hi"),
            ),
        ),
        ("container", body_with(r#","container":"c1""#, json!("hi"))),
        (
            "a server tool",
            body_with(
                r#","tools":[{"type":"web_search_20250305","name":"web_search"}]"#,
                json!("hi"),
            ),
        ),
        (
            "a web fetch tool",
            body_with(
                r#","tools":[{"type":"web_fetch_20250910","name":"web_fetch"}]"#,
                json!("hi"),
            ),
        ),
        (
            "an image from a URL",
            body_with("", json!([image(url.clone())])),
        ),
        (
            "an image from a file",
            body_with("", json!([image(file.clone())])),
        ),
        (
            "a document from a URL",
            body_with("", json!([{"type": "document", "source": url.clone()}])),
        ),
        (
            "a PDF",
            body_with(
                "",
                json!([{"type": "document", "source":
                    {"type": "base64", "media_type": "application/pdf", "data": "JVBE"}}]),
            ),
        ),
        (
            "a document from a file",
            body_with("", json!([{"type": "document", "source": file}])),
        ),
        (
            "a URL image inside a tool result",
            body_with(
                "",
                json!([{"type": "tool_result", "tool_use_id": "t1", "content": [image(url)]}]),
            ),
        ),
        (
            "a server tool block",
            body_with(
                "",
                json!([{"type": "server_tool_use", "id": "s1", "name": "web_search"}]),
            ),
        ),
        (
            "a one-hour cache write",
            body_with(
                "",
                json!([{"type": "text", "text": "hi",
                    "cache_control": {"type": "ephemeral", "ttl": "1h"}}]),
            ),
        ),
        (
            "too many images",
            body_with("", Value::Array(vec![image(b64); MAX_IMAGES as usize + 1])),
        ),
        (
            "content that is neither text nor blocks",
            body_with("", json!(7)),
        ),
        (
            "no messages",
            format!(r#"{{"model":"{MODEL}","max_tokens":10}}"#),
        ),
    ];
    for (what, body) in cases {
        assert_eq!(status(&g, body).await, 400, "{what}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_beta_header_is_refused() {
    let (up, calls) = upstream(SMALL).await;
    let g = gateway(&up, Setup::default()).await;
    let res = reqwest::Client::new()
        .post(format!("{}/v1/messages", g.url))
        .header("anthropic-beta", "web-fetch-2025-09-10")
        .body(msg(MODEL, 10))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_shapes_runners_send_are_forwarded() {
    let (up, calls) = upstream(SMALL).await;
    let g = gateway(&up, Setup::default()).await;
    let body = json!({
        "model": MODEL,
        "max_tokens": 10,
        "stream": false,
        "system": [{"type": "text", "text": "be brief", "cache_control": {"type": "ephemeral"}}],
        "tools": [{"name": "bash", "description": "run", "input_schema": {"type": "object"},
            "cache_control": {"type": "ephemeral", "ttl": "5m"}}],
        "tool_choice": {"type": "auto"},
        "thinking": {"type": "enabled", "budget_tokens": 5},
        "metadata": {"user_id": "u"},
        "messages": [
            {"role": "user", "content": [
                {"type": "text", "text": "look"},
                image(json!({"type": "base64", "media_type": "image/png", "data": "iVBO"})),
                {"type": "document", "source": {"type": "text", "media_type": "text/plain", "data": "notes"}},
            ]},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "hm", "signature": "s"},
                {"type": "tool_use", "id": "t1", "name": "bash", "input": {"cmd": "ls"}},
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": [{"type": "text", "text": "a"}]},
            ]},
        ],
    });
    assert_eq!(status(&g, body.to_string()).await, 200);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_image_reserves_its_largest_cost() {
    let (up, calls) = upstream(SMALL).await;
    let one = image(json!({"type": "base64", "media_type": "image/png", "data": "iVBO"}));
    let body = body_with("", json!([one]));
    let r = reservation(&common::PRICE, body.len(), 0, 10);
    // Room for the body's bytes, not for the image's pixels.
    let g = gateway(
        &up,
        Setup {
            run: run(
                &[CLASS],
                Budget {
                    max_tokens: r.tokens + IMAGE_TOKENS - 1,
                    max_cost_micro_usd: 1_000_000,
                },
            ),
            ..Setup::default()
        },
    )
    .await;
    assert_eq!(status(&g, body).await, 403);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn refusals_are_audit_records() {
    let (up, _) = upstream(SMALL).await;
    let g = gateway(&up, Setup::default()).await;
    assert_eq!(status(&g, msg("other-model", 10)).await, 403);
    assert_eq!(
        status(&g, body_with(r#","container":"c1""#, json!("hi"))).await,
        400
    );
    let e = entries(&g.db);
    let refusals: Vec<_> = e
        .iter()
        .filter(|(t, p)| t == "run_audit" && p["kind"] == "gateway_refusal")
        .map(|(_, p)| (p["event"].as_str().unwrap(), p["run"].as_str().unwrap()))
        .collect();
    assert_eq!(refusals, [("deny_model", "r1"), ("deny_shape", "r1")]);
    assert!(e[1].1["reason"].as_str().unwrap().contains("container"));
}

#[tokio::test]
async fn a_failed_audit_write_refuses_the_request() {
    let (up, calls) = upstream(SMALL).await;
    let g = gateway(&up, Setup::default()).await;
    // Hold the write lock past kod-audit's busy timeout.
    let lock = rusqlite::Connection::open(&g.db).unwrap();
    lock.execute_batch("BEGIN EXCLUSIVE").unwrap();
    assert_eq!(status(&g, msg(MODEL, 10)).await, 503);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    lock.execute_batch("COMMIT").unwrap();
    assert_eq!(status(&g, msg(MODEL, 10)).await, 200);
}
