//! Thin client for the LinkedIn self-serve member APIs:
//! - Sign In with LinkedIn using OpenID Connect (`GET /v2/userinfo`)
//! - Share on LinkedIn (`/v2/ugcPosts`, `/v2/assets`, `/v2/socialActions`), all under `w_member_social`

use std::{net::IpAddr, sync::Arc, time::Duration};

use reqwest::{StatusCode, header};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::OnceCell;

const API_BASE: &str = "https://api.linkedin.com";
const RESTLI_HEADER: &str = "X-Restli-Protocol-Version";

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct UserInfo {
    /// Member identifier; the person URN is `urn:li:person:{sub}`.
    pub sub: String,
    pub name: Option<String>,
    pub given_name: Option<String>,
    pub family_name: Option<String>,
    pub picture: Option<String>,
    pub locale: Option<Value>,
    pub email: Option<String>,
    pub email_verified: Option<bool>,
}

impl UserInfo {
    pub fn person_urn(&self) -> String {
        format!("urn:li:person:{}", self.sub)
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum Visibility {
    /// Viewable by anyone on LinkedIn.
    #[default]
    Public,
    /// Viewable by 1st-degree connections only.
    Connections,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    #[default]
    Image,
    Video,
}

impl MediaKind {
    fn recipe(self) -> &'static str {
        match self {
            MediaKind::Image => "urn:li:digitalmediaRecipe:feedshare-image",
            MediaKind::Video => "urn:li:digitalmediaRecipe:feedshare-video",
        }
    }

    fn category(self) -> &'static str {
        match self {
            MediaKind::Image => "IMAGE",
            MediaKind::Video => "VIDEO",
        }
    }

    /// Largest file the server will upload for this kind.
    pub fn max_bytes(self) -> usize {
        match self {
            MediaKind::Image => 20 * 1024 * 1024,
            MediaKind::Video => 200 * 1024 * 1024,
        }
    }
}

/// What a share carries besides its commentary.
#[derive(Debug, Clone)]
pub enum ShareMedia {
    Article {
        url: String,
        title: Option<String>,
        description: Option<String>,
    },
    /// An image or video already uploaded with `upload_media`.
    Asset {
        kind: MediaKind,
        asset_urn: String,
        title: Option<String>,
        description: Option<String>,
    },
}

#[derive(Clone)]
pub struct LinkedInClient {
    http: reqwest::Client,
    /// Used to download media from caller-supplied URLs: no redirects, no LinkedIn token.
    fetcher: reqwest::Client,
    token: Arc<str>,
    base: Arc<str>,
    me: Arc<OnceCell<UserInfo>>,
}

impl LinkedInClient {
    pub fn new(token: String) -> anyhow::Result<Self> {
        Self::with_base(token, API_BASE.into())
    }

    pub fn with_base(token: String, base: String) -> anyhow::Result<Self> {
        let user_agent = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));
        let http = reqwest::Client::builder()
            .user_agent(user_agent)
            .timeout(Duration::from_secs(300))
            .build()?;
        let fetcher = reqwest::Client::builder()
            .user_agent(user_agent)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(120))
            .build()?;
        Ok(Self {
            http,
            fetcher,
            token: token.into(),
            base: base.into(),
            me: Arc::new(OnceCell::new()),
        })
    }

    pub async fn userinfo(&self) -> Result<UserInfo, String> {
        let resp = send(
            self.http
                .get(format!("{}/v2/userinfo", self.base))
                .bearer_auth(&*self.token),
        )
        .await?;
        resp.json::<UserInfo>()
            .await
            .map_err(|e| format!("unexpected userinfo response: {e}"))
    }

    /// The authenticated member, fetched once and cached for later author URNs.
    pub async fn me(&self) -> Result<&UserInfo, String> {
        self.me.get_or_try_init(|| self.userinfo()).await
    }

    /// Creates a share and returns the new post URN from the `X-RestLi-Id` header.
    pub async fn create_post(
        &self,
        text: &str,
        visibility: Visibility,
        media: Option<&ShareMedia>,
    ) -> Result<String, String> {
        let author = self.me().await?.person_urn();
        let body = ugc_post_body(&author, text, visibility, media);
        let resp = send(
            self.http
                .post(format!("{}/v2/ugcPosts", self.base))
                .bearer_auth(&*self.token)
                .header(RESTLI_HEADER, "2.0.0")
                .json(&body),
        )
        .await?;
        Ok(restli_id(&resp))
    }

    /// Registers an upload, sends the bytes, and returns the `urn:li:digitalmediaAsset:…` URN.
    pub async fn upload_media(&self, kind: MediaKind, bytes: Vec<u8>) -> Result<String, String> {
        let owner = self.me().await?.person_urn();
        let resp = send(
            self.http
                .post(format!("{}/v2/assets?action=registerUpload", self.base))
                .bearer_auth(&*self.token)
                .header(RESTLI_HEADER, "2.0.0")
                .json(&register_upload_body(&owner, kind)),
        )
        .await?;
        let reg: Value = resp
            .json()
            .await
            .map_err(|e| format!("unexpected registerUpload response: {e}"))?;
        let value = &reg["value"];
        let mechanism =
            &value["uploadMechanism"]["com.linkedin.digitalmedia.uploading.MediaUploadHttpRequest"];
        let upload_url = mechanism["uploadUrl"]
            .as_str()
            .ok_or("registerUpload response has no uploadUrl")?;
        let asset = value["asset"]
            .as_str()
            .ok_or("registerUpload response has no asset URN")?
            .to_string();

        let mut req = self.http.put(upload_url).body(bytes);
        // Only send the member token back to LinkedIn's own upload host.
        if is_linkedin_host(upload_url) {
            req = req.bearer_auth(&*self.token);
        }
        if let Some(headers) = mechanism["headers"].as_object() {
            for (k, v) in headers {
                if let Some(v) = v.as_str() {
                    req = req.header(k.as_str(), v);
                }
            }
        }
        send(req).await?;

        if kind == MediaKind::Video {
            self.wait_until_available(&asset).await;
        }
        Ok(asset)
    }

    /// Videos are processed asynchronously; give LinkedIn up to two minutes before posting.
    async fn wait_until_available(&self, asset_urn: &str) {
        let id = asset_urn.rsplit(':').next().unwrap_or(asset_urn);
        for _ in 0..40 {
            let status = async {
                let resp = send(
                    self.http
                        .get(format!("{}/v2/assets/{}", self.base, encode_urn(id)))
                        .bearer_auth(&*self.token),
                )
                .await?;
                let v: Value = resp.json().await.map_err(|e| e.to_string())?;
                Ok::<_, String>(
                    v["recipes"]
                        .as_array()
                        .and_then(|r| r.first())
                        .and_then(|r| r["status"].as_str())
                        .unwrap_or_default()
                        .to_string(),
                )
            }
            .await;
            match status.as_deref() {
                Ok("AVAILABLE") => return,
                Ok("PROCESSING_FAILED") | Ok("CLIENT_ERROR") => {
                    tracing::warn!(asset_urn, "LinkedIn failed to process the video");
                    return;
                }
                Err(e) => tracing::debug!(asset_urn, error = %e, "asset status check failed"),
                _ => {}
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
        tracing::warn!(
            asset_urn,
            "video still processing after 120s; posting anyway"
        );
    }

    /// Adds a comment as the member and returns the new comment's id.
    pub async fn comment(&self, post_urn: &str, text: &str) -> Result<String, String> {
        let actor = self.me().await?.person_urn();
        let resp = send(
            self.http
                .post(format!(
                    "{}/v2/socialActions/{}/comments",
                    self.base,
                    encode_urn(post_urn)
                ))
                .bearer_auth(&*self.token)
                .header(RESTLI_HEADER, "2.0.0")
                .json(&json!({
                    "actor": actor,
                    "object": post_urn,
                    "message": { "text": text },
                })),
        )
        .await?;
        Ok(restli_id(&resp))
    }

    pub async fn like(&self, post_urn: &str) -> Result<(), String> {
        let actor = self.me().await?.person_urn();
        send(
            self.http
                .post(format!(
                    "{}/v2/socialActions/{}/likes",
                    self.base,
                    encode_urn(post_urn)
                ))
                .bearer_auth(&*self.token)
                .header(RESTLI_HEADER, "2.0.0")
                .json(&json!({ "actor": actor, "object": post_urn })),
        )
        .await?;
        Ok(())
    }

    /// Deletes one of the member's own posts. Idempotent: deleting twice returns success.
    pub async fn delete_post(&self, post_urn: &str) -> Result<(), String> {
        send(
            self.http
                .delete(format!(
                    "{}/v2/ugcPosts/{}",
                    self.base,
                    encode_urn(post_urn)
                ))
                .bearer_auth(&*self.token)
                .header(RESTLI_HEADER, "2.0.0"),
        )
        .await?;
        Ok(())
    }

    /// Downloads media from a public https URL, refusing private addresses and oversized files.
    pub async fn fetch_media(&self, raw_url: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
        let url = url::Url::parse(raw_url).map_err(|e| format!("invalid media_url: {e}"))?;
        if url.scheme() != "https" {
            return Err("media_url must use https".into());
        }
        let host = url.host_str().ok_or("media_url has no host")?;
        let port = url.port_or_known_default().unwrap_or(443);
        let addrs = tokio::net::lookup_host((host, port))
            .await
            .map_err(|e| format!("could not resolve {host}: {e}"))?;
        for addr in addrs {
            if !is_public_ip(addr.ip()) {
                return Err(format!(
                    "media_url resolves to a non-public address ({})",
                    addr.ip()
                ));
            }
        }

        let mut resp = self
            .fetcher
            .get(url)
            .send()
            .await
            .map_err(|e| format!("downloading media failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("downloading media failed: HTTP {}", resp.status()));
        }
        if resp
            .content_length()
            .is_some_and(|n| n as usize > max_bytes)
        {
            return Err(too_large(max_bytes));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| format!("downloading media failed: {e}"))?
        {
            bytes.extend_from_slice(&chunk);
            if bytes.len() > max_bytes {
                return Err(too_large(max_bytes));
            }
        }
        Ok(bytes)
    }
}

pub fn too_large(max_bytes: usize) -> String {
    format!("media is larger than {} MB", max_bytes / (1024 * 1024))
}

pub fn ugc_post_body(
    author: &str,
    text: &str,
    visibility: Visibility,
    media: Option<&ShareMedia>,
) -> Value {
    let mut share_content = json!({ "shareCommentary": { "text": text } });
    let (category, item, title, description) = match media {
        None => ("NONE", None, None, None),
        Some(ShareMedia::Article {
            url,
            title,
            description,
        }) => (
            "ARTICLE",
            Some(json!({ "status": "READY", "originalUrl": url })),
            title.as_ref(),
            description.as_ref(),
        ),
        Some(ShareMedia::Asset {
            kind,
            asset_urn,
            title,
            description,
        }) => (
            kind.category(),
            Some(json!({ "status": "READY", "media": asset_urn })),
            title.as_ref(),
            description.as_ref(),
        ),
    };
    share_content["shareMediaCategory"] = json!(category);
    if let Some(mut item) = item {
        if let Some(t) = title {
            item["title"] = json!({ "text": t });
        }
        if let Some(d) = description {
            item["description"] = json!({ "text": d });
        }
        share_content["media"] = json!([item]);
    }
    json!({
        "author": author,
        "lifecycleState": "PUBLISHED",
        "specificContent": { "com.linkedin.ugc.ShareContent": share_content },
        "visibility": { "com.linkedin.ugc.MemberNetworkVisibility": visibility },
    })
}

pub fn register_upload_body(owner: &str, kind: MediaKind) -> Value {
    json!({
        "registerUploadRequest": {
            "recipes": [kind.recipe()],
            "owner": owner,
            "serviceRelationships": [{
                "relationshipType": "OWNER",
                "identifier": "urn:li:userGeneratedContent"
            }]
        }
    })
}

/// Accepts a post URN (`urn:li:share:…`, `urn:li:ugcPost:…`, `urn:li:activity:…`) or a
/// linkedin.com post URL containing one, and returns the URN.
pub fn parse_post_urn(input: &str) -> Result<String, String> {
    let input = input.trim();
    for prefix in ["urn:li:share:", "urn:li:ugcPost:", "urn:li:activity:"] {
        if let Some(start) = input.find(prefix) {
            let id: String = input[start + prefix.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if !id.is_empty() {
                return Ok(format!("{prefix}{id}"));
            }
        }
    }
    // Share links look like linkedin.com/posts/<slug>-activity-<id>-<suffix>.
    if let Some(start) = input.find("-activity-") {
        let id: String = input[start + "-activity-".len()..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if !id.is_empty() {
            return Ok(format!("urn:li:activity:{id}"));
        }
    }
    Err(
        "post must be a post URN (urn:li:share:…, urn:li:ugcPost:… or urn:li:activity:…) \
         or a linkedin.com post URL"
            .into(),
    )
}

/// Percent-encodes a URN for use as a URL path segment (Rest.li 2.0 requires `:` → `%3A`).
pub fn encode_urn(urn: &str) -> String {
    let mut out = String::with_capacity(urn.len() * 3);
    for b in urn.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn is_linkedin_host(raw_url: &str) -> bool {
    url::Url::parse(raw_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .is_some_and(|h| h == "linkedin.com" || h.ends_with(".linkedin.com"))
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (64..128).contains(&o[1])) // carrier-grade NAT
                || o[0] >= 224)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(v4));
            }
            let seg = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg[0] & 0xfe00) == 0xfc00 // unique local
                || (seg[0] & 0xffc0) == 0xfe80) // link local
        }
    }
}

fn restli_id(resp: &reqwest::Response) -> String {
    resp.headers()
        .get("x-restli-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

async fn send(req: reqwest::RequestBuilder) -> Result<reqwest::Response, String> {
    let resp = req
        .send()
        .await
        .map_err(|e| format!("request to LinkedIn failed: {e}"))?;
    check(resp).await
}

async fn check(resp: reqwest::Response) -> Result<reqwest::Response, String> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let retry_after = resp
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let body = resp.text().await.unwrap_or_default();
    let detail = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| v.get("message").and_then(Value::as_str).map(String::from))
        .unwrap_or(body);
    let hint = match status {
        StatusCode::UNAUTHORIZED => {
            "the LinkedIn access token is invalid or expired; generate a new one".to_string()
        }
        StatusCode::FORBIDDEN => "LinkedIn refused the request: the token may lack a scope \
             (openid, profile, email, w_member_social), or the post isn't yours to change"
            .to_string(),
        StatusCode::NOT_FOUND => "LinkedIn could not find that post".to_string(),
        StatusCode::TOO_MANY_REQUESTS => format!(
            "LinkedIn rate limit reached (150 requests/member/day){}",
            retry_after
                .map(|s| format!("; retry after {s}s"))
                .unwrap_or_default()
        ),
        _ => "LinkedIn API error".to_string(),
    };
    Err(format!("{hint} (HTTP {status}): {detail}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_post_body_matches_documented_shape() {
        let body = ugc_post_body("urn:li:person:abc", "Hello", Visibility::Public, None);
        assert_eq!(
            body,
            json!({
                "author": "urn:li:person:abc",
                "lifecycleState": "PUBLISHED",
                "specificContent": {
                    "com.linkedin.ugc.ShareContent": {
                        "shareCommentary": { "text": "Hello" },
                        "shareMediaCategory": "NONE"
                    }
                },
                "visibility": { "com.linkedin.ugc.MemberNetworkVisibility": "PUBLIC" }
            })
        );
    }

    #[test]
    fn article_post_body_includes_media() {
        let media = ShareMedia::Article {
            url: "https://example.com".into(),
            title: Some("Title".into()),
            description: None,
        };
        let body = ugc_post_body(
            "urn:li:person:abc",
            "Read this",
            Visibility::Connections,
            Some(&media),
        );
        let content = &body["specificContent"]["com.linkedin.ugc.ShareContent"];
        assert_eq!(content["shareMediaCategory"], "ARTICLE");
        assert_eq!(
            content["media"],
            json!([{
                "status": "READY",
                "originalUrl": "https://example.com",
                "title": { "text": "Title" }
            }])
        );
        assert_eq!(
            body["visibility"]["com.linkedin.ugc.MemberNetworkVisibility"],
            "CONNECTIONS"
        );
    }

    #[test]
    fn image_post_body_references_asset() {
        let media = ShareMedia::Asset {
            kind: MediaKind::Image,
            asset_urn: "urn:li:digitalmediaAsset:C5422AQEbc381YmIuvg".into(),
            title: None,
            description: Some("Center stage!".into()),
        };
        let body = ugc_post_body("urn:li:person:abc", "Hi", Visibility::Public, Some(&media));
        let content = &body["specificContent"]["com.linkedin.ugc.ShareContent"];
        assert_eq!(content["shareMediaCategory"], "IMAGE");
        assert_eq!(
            content["media"],
            json!([{
                "status": "READY",
                "media": "urn:li:digitalmediaAsset:C5422AQEbc381YmIuvg",
                "description": { "text": "Center stage!" }
            }])
        );
    }

    #[test]
    fn register_upload_body_matches_documented_shape() {
        assert_eq!(
            register_upload_body("urn:li:person:8675309", MediaKind::Video),
            json!({
                "registerUploadRequest": {
                    "recipes": ["urn:li:digitalmediaRecipe:feedshare-video"],
                    "owner": "urn:li:person:8675309",
                    "serviceRelationships": [{
                        "relationshipType": "OWNER",
                        "identifier": "urn:li:userGeneratedContent"
                    }]
                }
            })
        );
    }

    #[test]
    fn parses_post_urns_and_urls() {
        assert_eq!(
            parse_post_urn("urn:li:share:123").unwrap(),
            "urn:li:share:123"
        );
        assert_eq!(
            parse_post_urn("https://www.linkedin.com/feed/update/urn:li:activity:7123/").unwrap(),
            "urn:li:activity:7123"
        );
        assert_eq!(
            parse_post_urn("https://www.linkedin.com/posts/jane_rust-activity-7246-AbCd").unwrap(),
            "urn:li:activity:7246"
        );
        assert!(parse_post_urn("https://example.com/post/1").is_err());
        assert!(parse_post_urn("urn:li:share:").is_err());
    }

    #[test]
    fn encodes_urns_for_paths() {
        assert_eq!(
            encode_urn("urn:li:ugcPost:12345"),
            "urn%3Ali%3AugcPost%3A12345"
        );
    }

    #[test]
    fn rejects_non_public_ips() {
        for ip in [
            "127.0.0.1",
            "10.0.0.5",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "::1",
            "fd00::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(!is_public_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "2606:4700:4700::1111"] {
            assert!(is_public_ip(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn token_only_sent_to_linkedin_hosts() {
        assert!(is_linkedin_host("https://api.linkedin.com/mediaUpload/x"));
        assert!(!is_linkedin_host("https://evil-linkedin.com/x"));
        assert!(!is_linkedin_host("https://linkedin.com.evil.io/x"));
    }
}
