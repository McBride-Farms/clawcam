use anyhow::Result;
use serde::Serialize;
use std::net::IpAddr;
use std::time::Duration;

#[derive(Debug, Clone, Serialize)]
pub struct Detection {
    pub class: String,
    pub class_id: u32,
    pub score: f32,
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

#[derive(Debug, Serialize)]
pub struct WebhookPayload {
    pub ts: String,
    pub epoch: i64,
    #[serde(rename = "type")]
    pub event_type: String,
    pub detail: String,
    pub source: String,
    pub host: String,
    pub image: String,
    pub predictions: Vec<Detection>,
}

const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn send(
    url: &str,
    token: Option<&str>,
    payload: &WebhookPayload,
) -> Result<()> {
    // Reject plaintext HTTP when a bearer token is configured,
    // unless the target is a private/local network address (RFC1918, loopback).
    if token.is_some() && url.starts_with("http://") && !is_private_url(url) {
        anyhow::bail!(
            "refusing to send bearer token over plaintext HTTP — use https:// for webhook URL"
        );
    }

    let client = reqwest::Client::builder()
        .timeout(WEBHOOK_TIMEOUT)
        .build()?;
    let mut req = client.post(url).json(payload);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await?;
    if !resp.status().is_success() {
        tracing::warn!("webhook returned {}: {}", resp.status(), resp.text().await.unwrap_or_default());
    } else {
        tracing::info!("webhook delivered successfully");
    }
    Ok(())
}

/// Check if a URL points to a private/local network address (safe for plaintext HTTP).
fn is_private_url(url: &str) -> bool {
    let host = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .and_then(|s| s.split('/').next())
        .and_then(|s| s.split(':').next())
        .unwrap_or("");
    if host == "localhost" {
        return true;
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return match ip {
            IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
            IpAddr::V6(v6) => v6.is_loopback(),
        };
    }
    // .local mDNS hostnames are LAN-only
    host.ends_with(".local")
}
