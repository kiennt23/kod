use super::fixture::Remote;
use axum::{
    body::{Body, to_bytes},
    extract::{Request, State},
    http::StatusCode,
    response::Response,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
fn response(status: u16, body: Value) -> Response {
    Response::builder()
        .status(status)
        .body(Body::from(body.to_string()))
        .unwrap()
}
pub(super) async fn handler(State(state): State<Arc<Mutex<Remote>>>, request: Request) -> Response {
    let method = request.method().to_string();
    let uri = request.uri().clone();
    let token = request
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .trim_start_matches("Bearer ")
        .to_owned();
    let path = uri.path().replace("%2F", "/").replace("%23", "#");
    let query = uri.query().unwrap_or("").to_owned();
    let bytes = to_bytes(request.into_body(), 16 << 20).await.unwrap();
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    let state2 = state.clone();
    tokio::task::spawn_blocking(move || {
        let mut s=state2.lock().unwrap(); s.requests.push((method.clone(),path.clone(),body.clone()));
        if method=="POST" && path=="/app/installations/2/access_tokens" {
            super::objects::verify_jwt(&s.public_key,&token);
            assert_eq!(body,json!({"repository_ids":[1],"permissions":{"contents":"write","pull_requests":"write"}}));
            s.issued+=1; let t=format!("ghs_fixture{:032}",s.issued); s.live.insert(t.clone());
            if s.lost_mint {return response(503,json!({"message":t}));}
            let mut permissions=json!({"contents":"write","pull_requests":"write","metadata":"read"});
            if s.extra_permission {permissions["administration"]=json!("write");}
            let result=response(201,json!({"token":t,"expires_at":"2027-01-15T09:00:00Z","permissions":permissions,
                "repositories":[{"id":if s.widened {99}else{1},"full_name":"acme/app"}]}));
            let hook=s.after_mint.take();drop(s);if let Some(hook)=hook{hook();}return result;
        }
        if !s.live.contains(&token) {return response(401,json!({}));}
        if method=="DELETE" && path=="/installation/token" {
            if s.fail_revoke {return response(503,json!({"message":token}));}
            s.live.remove(&token);return response(204,Value::Null);
        }
        if method=="GET" && path=="/repos/acme/app" {return response(200,json!({"id":1,"full_name":"acme/app","default_branch":"main"}));}
        if method=="GET" && path=="/repos/acme/app/rulesets/3" {return response(200,json!({"id":3,"target":"branch","enforcement":if s.disabled {"disabled"}else{"active"},"bypass_actors":if s.app_bypass {json!([{"actor_id":5,"actor_type":"Integration","bypass_mode":"always"}])}else{json!([])},"conditions":{"ref_name":{"include":["~ALL"],"exclude":["refs/heads/agent/*","refs/heads/agent/**/*"]}},"rules":[{"type":"creation"},{"type":"update"},{"type":"deletion"}]}));}
        if path.contains("/git/") && !path.contains("/ref") {return super::objects::handle(&mut s,&method,&path,&body);}
        let prefix=if method=="GET" {"/repos/acme/app/git/ref/heads/"}else{"/repos/acme/app/git/refs/heads/"};
        if let Some(branch)=path.strip_prefix(prefix) {
            if method=="GET" {return match s.refs.get(branch) {Some(sha)=>response(200,json!({"ref":format!("refs/heads/{branch}"),"object":{"type":"commit","sha":sha}})),None=>response(404,json!({}))};}
            if method=="DELETE" {s.refs.remove(branch);return response(204,Value::Null);}
        }
        if method=="POST" && path=="/repos/acme/app/git/refs" {
            let branch=body["ref"].as_str().unwrap().trim_start_matches("refs/heads/").to_owned();
            if s.refs.contains_key(&branch) {return response(422,json!({}));}
            s.refs.insert(branch,body["sha"].as_str().unwrap().into());
            let hook=s.after_ref.take();let lost=s.lost_ref; drop(s);if let Some(h)=hook{h();}
            return response(if lost {503}else{201},body);
        }
        if path=="/repos/acme/app/pulls" {
            if method=="GET" {
                if s.hide_pr {return response(200,json!([]));}
                let mut prs=s.prs.clone();
                if s.foreign {for p in &mut prs {p["user"]["id"]=json!(9);}}
                if s.ambiguous && !prs.is_empty() {prs.push(prs[0].clone());}
                if s.paginated && query.split('&').any(|part|part=="page=1") {
                    let origin=if s.foreign_link {"https://evil.invalid/"}else{&s.api};
                    return Response::builder().status(200).header("link",format!("<{origin}repos/acme/app/pulls?page=2>; rel=\"next\""))
                        .body(Body::from("[]")).unwrap();
                }
                return response(200,json!(prs));
            }
            if method=="POST" {
                let branch=body["head"].as_str().unwrap();let sha=s.refs.get(branch).unwrap().clone();
                let pr=json!({"number":1,"html_url":"https://github.com/acme/app/pull/1","state":"open","draft":body["draft"],
                    "user":{"id":4},"head":{"repo":{"id":1},"ref":branch,"sha":sha},"base":{"repo":{"id":1},"ref":body["base"]}});
                s.prs.push(pr.clone()); return response(if s.lost_pr {503}else{201},pr);
            }
        }
        response(StatusCode::NOT_FOUND.as_u16(),json!({}))
    }).await.unwrap()
}
