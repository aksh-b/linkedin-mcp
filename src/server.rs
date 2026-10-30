use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Json, wrapper::Parameters},
    model::{Implementation, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::linkedin::{
    LinkedInClient, MediaKind, ShareMedia, UserInfo, Visibility, parse_post_urn, too_large,
};

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

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateMediaPostRequest {
    /// Post text shown above the media.
    pub text: String,
    /// Kind of media being attached. Defaults to image.
    #[serde(default)]
    pub media_type: MediaKind,
    /// Public https URL of the image or video to download and attach.
    pub media_url: Option<String>,
    /// The image or video file as base64 (a data: URL prefix is accepted).
    pub media_base64: Option<String>,
    /// Optional title for the media.
    pub title: Option<String>,
    /// Optional description (alt text) for the media.
    pub description: Option<String>,
    /// Who can see the post. Defaults to PUBLIC.
    #[serde(default)]
    pub visibility: Visibility,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CommentRequest {
    /// The post to comment on: its URN (urn:li:share:…, urn:li:ugcPost:… or
    /// urn:li:activity:…) or its linkedin.com URL.
    pub post: String,
    /// Comment text, up to 1250 characters.
    pub text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PostRef {
    /// The post: its URN (urn:li:share:…, urn:li:ugcPost:… or urn:li:activity:…) or its
    /// linkedin.com URL.
    pub post: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CommentCreated {
    /// URN of the post that was commented on.
    pub post_urn: String,
    /// Id of the new comment.
    pub comment_id: String,
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
        annotations(
            title = "Get LinkedIn profile",
            read_only_hint = true,
            open_world_hint = true
        )
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
        let urn = self
            .client
            .create_post(&req.text, req.visibility, None)
            .await?;
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
        let article = ShareMedia::Article {
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

    #[tool(
        description = "Publish a post with an image or video to the authenticated member's LinkedIn feed. Provide the file as either media_url (public https URL) or media_base64. Images up to 20 MB, videos up to 200 MB. This posts immediately.",
        annotations(
            title = "Create LinkedIn image or video post",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn create_media_post(
        &self,
        Parameters(req): Parameters<CreateMediaPostRequest>,
    ) -> Result<Json<PostCreated>, String> {
        validate_text(&req.text)?;
        let max = req.media_type.max_bytes();
        let bytes = match (req.media_url, req.media_base64) {
            (Some(url), None) => self.client.fetch_media(&url, max).await?,
            (None, Some(data)) => {
                // Tolerate a data: URL prefix and line breaks.
                let data = data.rsplit(',').next().unwrap_or_default();
                let data: String = data.chars().filter(|c| !c.is_whitespace()).collect();
                let bytes = BASE64
                    .decode(data)
                    .map_err(|e| format!("media_base64 is not valid base64: {e}"))?;
                if bytes.len() > max {
                    return Err(too_large(max));
                }
                bytes
            }
            _ => return Err("provide exactly one of media_url or media_base64".into()),
        };
        if bytes.is_empty() {
            return Err("media is empty".into());
        }
        let asset_urn = self.client.upload_media(req.media_type, bytes).await?;
        let media = ShareMedia::Asset {
            kind: req.media_type,
            asset_urn,
            title: req.title,
            description: req.description,
        };
        let urn = self
            .client
            .create_post(&req.text, req.visibility, Some(&media))
            .await?;
        Ok(Json(PostCreated::new(urn)))
    }

    #[tool(
        description = "Comment on a LinkedIn post as the authenticated member. This publishes the comment immediately.",
        annotations(
            title = "Comment on LinkedIn post",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn comment_on_post(
        &self,
        Parameters(req): Parameters<CommentRequest>,
    ) -> Result<Json<CommentCreated>, String> {
        let post_urn = parse_post_urn(&req.post)?;
        let text = req.text.trim();
        if text.is_empty() {
            return Err("text must not be empty".into());
        }
        if text.chars().count() > 1250 {
            return Err("comment text must be at most 1250 characters".into());
        }
        let comment_id = self.client.comment(&post_urn, text).await?;
        Ok(Json(CommentCreated {
            post_urn,
            comment_id,
        }))
    }

    #[tool(
        description = "Like a LinkedIn post as the authenticated member.",
        annotations(
            title = "Like LinkedIn post",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn like_post(&self, Parameters(req): Parameters<PostRef>) -> Result<String, String> {
        let post_urn = parse_post_urn(&req.post)?;
        self.client.like(&post_urn).await?;
        Ok(format!("Liked {post_urn}"))
    }

    #[tool(
        description = "Permanently delete one of the authenticated member's own LinkedIn posts. This cannot be undone.",
        annotations(
            title = "Delete LinkedIn post",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    async fn delete_post(&self, Parameters(req): Parameters<PostRef>) -> Result<String, String> {
        let post_urn = parse_post_urn(&req.post)?;
        if post_urn.starts_with("urn:li:activity:") {
            return Err(
                "delete_post needs the post's urn:li:share:… or urn:li:ugcPost:… URN \
                        (as returned when the post was created), not an activity URN"
                    .into(),
            );
        }
        self.client.delete_post(&post_urn).await?;
        Ok(format!("Deleted {post_urn}"))
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
                 create_text_post, share_article, create_media_post and comment_on_post \
                 publish immediately, so confirm the exact text, media and visibility with \
                 the user first. delete_post is permanent; confirm before calling it.",
            )
    }
}
