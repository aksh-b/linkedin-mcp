use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Json, wrapper::Parameters},
    model::{Implementation, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::linkedin::{Article, LinkedInClient, UserInfo, Visibility};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateTextPostRequest {
    /// Post text. Plain text; hashtags like #rust are kept as written.
    pub text: String,
    /// Who can see the post. Defaults to PUBLIC.
    #[serde(default)]
    pub visibility: Visibility,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ShareArticleRequest {
    /// Commentary shown above the link.
    pub text: String,
    /// URL of the article to share.
    pub url: String,
    /// Optional title shown on the link card.
    pub title: Option<String>,
    /// Optional description shown on the link card.
    pub description: Option<String>,
    /// Who can see the post. Defaults to PUBLIC.
    #[serde(default)]
    pub visibility: Visibility,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Profile {
    /// Person URN, used as the author of posts.
    pub person_urn: String,
    #[serde(flatten)]
    pub info: UserInfo,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PostCreated {
    /// URN of the new post, e.g. urn:li:share:123 or urn:li:ugcPost:123.
    pub post_urn: String,
    /// Link to view the post on linkedin.com.
    pub url: String,
}

impl PostCreated {
    fn new(post_urn: String) -> Self {
        let url = format!("https://www.linkedin.com/feed/update/{post_urn}/");
        Self { post_urn, url }
    }
}

#[derive(Clone)]
pub struct LinkedInMcp {
    client: LinkedInClient,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl LinkedInMcp {
    pub fn new(client: LinkedInClient) -> Self {
        Self {
            client,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Get the authenticated LinkedIn member's profile: name, email, picture, and person URN.",
        annotations(title = "Get LinkedIn profile", read_only_hint = true, open_world_hint = true)
    )]
    async fn get_profile(&self) -> Result<Json<Profile>, String> {
        let info = self.client.userinfo().await?;
        Ok(Json(Profile {
            person_urn: info.person_urn(),
            info,
        }))
    }

    #[tool(
        description = "Publish a text-only post to the authenticated member's LinkedIn feed. This posts publicly (or to connections) immediately.",
        annotations(
            title = "Create LinkedIn text post",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn create_text_post(
        &self,
        Parameters(req): Parameters<CreateTextPostRequest>,
    ) -> Result<Json<PostCreated>, String> {
        validate_text(&req.text)?;
        let urn = self.client.create_post(&req.text, req.visibility, None).await?;
        Ok(Json(PostCreated::new(urn)))
    }

    #[tool(
        description = "Share a link (article) with commentary to the authenticated member's LinkedIn feed. This posts immediately.",
        annotations(
            title = "Share article on LinkedIn",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn share_article(
        &self,
        Parameters(req): Parameters<ShareArticleRequest>,
    ) -> Result<Json<PostCreated>, String> {
        validate_text(&req.text)?;
        if !(req.url.starts_with("https://") || req.url.starts_with("http://")) {
            return Err("url must start with http:// or https://".into());
        }
        let article = Article {
            url: req.url,
            title: req.title,
            description: req.description,
        };
        let urn = self
            .client
            .create_post(&req.text, req.visibility, Some(&article))
            .await?;
        Ok(Json(PostCreated::new(urn)))
    }
}

/// LinkedIn caps share commentary at 3000 characters.
fn validate_text(text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("text must not be empty".into());
    }
    if text.chars().count() > 3000 {
        return Err("text must be at most 3000 characters".into());
    }
    Ok(())
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for LinkedInMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "Tools for the authenticated LinkedIn member. get_profile is read-only. \
                 create_text_post and share_article publish immediately, so confirm the \
                 exact text and visibility with the user before calling them.",
            )
    }
}
