mod config;
mod linkedin;
mod server;

use std::sync::Arc;

use axum::{
    Router,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use subtle::ConstantTimeEq;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use crate::{config::Config, linkedin::LinkedInClient, server::LinkedInMcp};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .init();

    let config = Config::from_env()?;
    let client = LinkedInClient::new(config.linkedin_access_token.clone())?;
    let ct = CancellationToken::new();

    let app = app(&config, client, ct.clone());
    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;
    tracing::info!("LinkedIn MCP server listening on http://{}/mcp", config.bind_addr);

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutting down");
            ct.cancel();
        })
        .await?;
    Ok(())
}

fn app(config: &Config, client: LinkedInClient, ct: CancellationToken) -> Router {
    let mut http_config = StreamableHttpServerConfig::default();
    http_config.cancellation_token = ct.child_token();
    http_config
        .allowed_hosts
        .extend(config.allowed_hosts.iter().cloned());

    let mcp: StreamableHttpService<LinkedInMcp, LocalSessionManager> = StreamableHttpService::new(
        move || Ok(LinkedInMcp::new(client.clone())),
        Default::default(),
        http_config,
    );

    let api_key: Arc<str> = config.mcp_api_key.clone().into();
    let protected = Router::new()
        .nest_service("/mcp", mcp)
        .layer(middleware::from_fn_with_state(api_key, require_api_key));

    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .merge(protected)
}

async fn require_api_key(State(key): State<Arc<str>>, req: Request, next: Next) -> Response {
    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    if presented.as_bytes().ct_eq(key.as_bytes()).into() {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            "missing or invalid bearer token",
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use tower::ServiceExt;

    fn test_app() -> Router {
        let config = Config {
            linkedin_access_token: "unused".into(),
            mcp_api_key: "test-key-0123456789".into(),
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            allowed_hosts: vec![],
        };
        let client = LinkedInClient::new("unused".into()).unwrap();
        app(&config, client, CancellationToken::new())
    }

    fn mcp_request(auth: Option<&str>) -> Request<Body> {
        let mut b = Request::post("/mcp")
            .header(header::HOST, "127.0.0.1")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream");
        if let Some(a) = auth {
            b = b.header(header::AUTHORIZATION, a);
        }
        b.body(Body::from(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
        ))
        .unwrap()
    }

    #[tokio::test]
    async fn healthz_is_open() {
        let resp = test_app()
            .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn mcp_rejects_missing_or_wrong_key() {
        for auth in [None, Some("Bearer wrong"), Some("test-key-0123456789")] {
            let resp = test_app().oneshot(mcp_request(auth)).await.unwrap();
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "auth={auth:?}");
        }
    }

    #[tokio::test]
    async fn mcp_accepts_valid_key() {
        let resp = test_app()
            .oneshot(mcp_request(Some("Bearer test-key-0123456789")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
