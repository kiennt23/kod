use anyhow::{Context, Result, ensure};
use reqwest::{
    Method, StatusCode,
    blocking::Client,
    header::{HeaderMap, HeaderValue},
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{io::Read, time::Duration};
pub(super) struct Http {
    client: Client,
    origin: reqwest::Url,
}
pub(super) struct Reply {
    pub status: StatusCode,
    pub body: Vec<u8>,
    pub link: Option<String>,
}
impl Reply {
    pub fn json<T: DeserializeOwned>(&self) -> Result<T> {
        ensure!(
            self.status.is_success(),
            "GitHub provider status {}",
            self.status.as_u16()
        );
        serde_json::from_slice(&self.body)
            .map_err(|_| anyhow::anyhow!("invalid GitHub provider response"))
    }
}
impl Http {
    pub fn new(api: &str) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-GitHub-Api-Version",
            HeaderValue::from_static("2026-03-10"),
        );
        headers.insert(
            "Accept",
            HeaderValue::from_static("application/vnd.github+json"),
        );
        let client = Client::builder()
            .default_headers(headers)
            .user_agent("kod-tool-gateway")
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .context("GitHub HTTP client unavailable")?;
        Ok(Self {
            client,
            origin: reqwest::Url::parse(api).context("invalid API origin")?,
        })
    }
    pub fn request(
        &self,
        method: Method,
        token: &str,
        path: &[String],
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<Reply> {
        let mut url = self.origin.clone();
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("invalid API origin"))?
            .clear()
            .extend(path);
        let mut auth = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| anyhow::anyhow!("invalid GitHub credential"))?;
        auth.set_sensitive(true);
        let mut req = self
            .client
            .request(method, url)
            .header(reqwest::header::AUTHORIZATION, auth)
            .query(query);
        if let Some(body) = body {
            req = req.json(body);
        }
        let mut response = req
            .send()
            .map_err(|_| anyhow::anyhow!("GitHub transport failed"))?;
        let status = response.status();
        let link = response
            .headers()
            .get(reqwest::header::LINK)
            .map(|v| v.to_str().map(str::to_owned))
            .transpose()
            .map_err(|_| anyhow::anyhow!("invalid GitHub pagination"))?;
        let mut body = Vec::new();
        response
            .by_ref()
            .take((8 << 20) + 1)
            .read_to_end(&mut body)
            .map_err(|_| anyhow::anyhow!("GitHub response read failed"))?;
        ensure!(body.len() <= 8 << 20, "GitHub response too large");
        Ok(Reply { status, body, link })
    }
    pub fn next_page(&self, link: Option<&str>, path: &[String], page: u32) -> Result<bool> {
        let Some(link) = link else { return Ok(false) };
        let mut next = None;
        for part in link.split(',') {
            let Some((target, rel)) = part.trim().split_once(';') else {
                anyhow::bail!("invalid GitHub pagination")
            };
            if rel.trim() == "rel=\"next\"" {
                ensure!(next.is_none(), "duplicate next page");
                let u = reqwest::Url::parse(
                    target.trim().trim_start_matches('<').trim_end_matches('>'),
                )
                .context("invalid pagination URL")?;
                let mut expected = self.origin.clone();
                expected
                    .path_segments_mut()
                    .map_err(|_| anyhow::anyhow!("invalid origin"))?
                    .clear()
                    .extend(path);
                ensure!(
                    u.origin() == expected.origin()
                        && u.path() == expected.path()
                        && u.username().is_empty()
                        && u.password().is_none()
                        && u.fragment().is_none(),
                    "foreign GitHub pagination"
                );
                let pairs: Vec<_> = u.query_pairs().collect();
                ensure!(
                    pairs
                        .iter()
                        .filter(|(k, v)| k == "page" && v == &(page + 1).to_string())
                        .count()
                        == 1,
                    "inconsistent pagination"
                );
                next = Some(());
            }
        }
        ensure!(page < 100 || next.is_none(), "GitHub pagination limit");
        Ok(next.is_some())
    }
}
