use std::net::SocketAddr;

use anyhow::{Context, Result, bail};

/// Runtime configuration, read from environment variables.
#[derive(Clone)]
pub struct Config {
    /// Member access token with `openid profile email w_member_social` scopes.
    pub linkedin_access_token: String,
    /// Shared secret clients must send as `Authorization: Bearer <key>` to reach `/mcp`.
    pub mcp_api_key: String,
    pub bind_addr: SocketAddr,
    /// Extra hostnames accepted in the `Host` header (loopback is always allowed).
    pub allowed_hosts: Vec<String>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let linkedin_access_token = required("LINKEDIN_ACCESS_TOKEN")?;
        let mcp_api_key = required("MCP_API_KEY")?;
        if mcp_api_key.len() < 16 {
            bail!("MCP_API_KEY must be at least 16 characters");
        }
        let bind_addr = std::env::var("BIND_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:8080".into())
            .parse()
            .context("BIND_ADDR must be a socket address like 127.0.0.1:8080")?;
        let allowed_hosts = std::env::var("ALLOWED_HOSTS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .map(String::from)
            .collect();
        Ok(Self {
            linkedin_access_token,
            mcp_api_key,
            bind_addr,
            allowed_hosts,
        })
    }
}

fn required(name: &str) -> Result<String> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Ok(v.trim().to_string()),
        _ => bail!("environment variable {name} is required"),
    }
}
