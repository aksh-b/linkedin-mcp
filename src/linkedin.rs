//! Thin client for the LinkedIn self-serve member APIs:
//! - Sign In with LinkedIn using OpenID Connect (`GET /v2/userinfo`)
//! - Share on LinkedIn (`POST /v2/ugcPosts`)

use std::sync::Arc;

use reqwest::{StatusCode, header};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::OnceCell;

const API_BASE: &str = "https://api.linkedin.com";

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

/// Article attachment for a share.
#[derive(Debug, Clone)]
pub struct Article {
    pub url: String,
    pub title: Option<String>,
    pub description: Option<String>,
}

#[derive(Clone)]
pub struct LinkedInClient {
    http: reqwest::Client,
    token: Arc<str>,
    base: Arc<str>,
    me: Arc<OnceCell<UserInfo>>,
}

impl LinkedInClient {
    pub fn new(token: String) -> anyhow::Result<Self> {
        Self::with_base(token, API_BASE.into())
    }

    pub fn with_base(token: String, base: String) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Self {
            http,
            token: token.into(),
            base: base.into(),
            me: Arc::new(OnceCell::new()),
        })
    }

    pub async fn userinfo(&self) -> Result<UserInfo, String> {
        let resp = self
            .http
            .get(format!("{}/v2/userinfo", self.base))
            .bearer_auth(&*self.token)
            .send()
            .await
            .map_err(|e| format!("request to LinkedIn failed: {e}"))?;
        let resp = check(resp).await?;
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
        article: Option<&Article>,
    ) -> Result<String, String> {
        let author = self.me().await?.person_urn();
        let body = ugc_post_body(&author, text, visibility, article);
        let resp = self
            .http
            .post(format!("{}/v2/ugcPosts", self.base))
            .bearer_auth(&*self.token)
            .header("X-Restli-Protocol-Version", "2.0.0")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("request to LinkedIn failed: {e}"))?;
        let resp = check(resp).await?;
        Ok(resp
            .headers()
            .get("x-restli-id")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string())
    }
}

pub fn ugc_post_body(
    author: &str,
    text: &str,
    visibility: Visibility,
    article: Option<&Article>,
) -> Value {
    let (category, media) = match article {
        None => ("NONE", None),
        Some(a) => {
            let mut m = json!({ "status": "READY", "originalUrl": a.url });
            if let Some(t) = &a.title {
                m["title"] = json!({ "text": t });
            }
            if let Some(d) = &a.description {
                m["description"] = json!({ "text": d });
            }
            ("ARTICLE", Some(vec![m]))
        }
    };
    let mut share_content = json!({
        "shareCommentary": { "text": text },
        "shareMediaCategory": category,
    });
    if let Some(media) = media {
        share_content["media"] = json!(media);
    }
    json!({
        "author": author,
        "lifecycleState": "PUBLISHED",
        "specificContent": { "com.linkedin.ugc.ShareContent": share_content },
        "visibility": { "com.linkedin.ugc.MemberNetworkVisibility": visibility },
    })
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
        StatusCode::FORBIDDEN => {
            "the token lacks a required scope (openid, profile, email, w_member_social)".to_string()
        }
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
        let article = Article {
            url: "https://example.com".into(),
            title: Some("Title".into()),
            description: None,
        };
        let body = ugc_post_body(
            "urn:li:person:abc",
            "Read this",
            Visibility::Connections,
            Some(&article),
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
}
