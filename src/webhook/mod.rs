use anyhow::{Result, bail};
use serde::Serialize;
use std::net::IpAddr;
use std::time::Duration;

/// One webhook destination. Multiple targets are sent to in parallel.
#[derive(Debug, Clone)]
pub struct WebhookTarget {
    pub url: String,
    pub token: Option<String>,
}

/// Pair a list of URLs with a list of tokens. Rules:
/// - 0 URLs → error.
/// - N URLs, 0 tokens → every target gets `None`.
/// - N URLs, 1 token → that token is broadcast to every URL.
/// - N URLs, N tokens → paired by index; an empty-string slot means `None`.
/// - any other token count → error.
pub fn parse_targets(urls: &[String], tokens: &[String]) -> Result<Vec<WebhookTarget>> {
    if urls.is_empty() {
        bail!("at least one webhook URL is required");
    }
    let mut targets = Vec::with_capacity(urls.len());
    match tokens.len() {
        0 => {
            for u in urls {
                targets.push(WebhookTarget { url: u.clone(), token: None });
            }
        }
        1 => {
            let shared = &tokens[0];
            let token = if shared.is_empty() { None } else { Some(shared.clone()) };
            for u in urls {
                targets.push(WebhookTarget { url: u.clone(), token: token.clone() });
            }
        }
        n if n == urls.len() => {
            for (u, t) in urls.iter().zip(tokens.iter()) {
                let token = if t.is_empty() { None } else { Some(t.clone()) };
                targets.push(WebhookTarget { url: u.clone(), token });
            }
        }
        n => bail!(
            "webhook token count ({n}) must be 0, 1, or {} (one per --webhook)",
            urls.len()
        ),
    }
    Ok(targets)
}

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

#[derive(Debug, Clone, Serialize)]
pub struct TrackInfo {
    pub track_id: u64,
    pub class: String,
    pub duration_secs: f64,
    pub movement_px: f32,
    pub is_stationary: bool,
    pub bbox: [u32; 4],
}

/// Per-inference bbox sample tagged to a position in the assembled clip.
/// `t` is playback seconds from the start of the clip (clip is assembled at
/// a fixed 10 FPS, so `t = frame_index / 10.0`).
#[derive(Debug, Clone, Serialize)]
pub struct ClipPredSample {
    pub frame_index: usize,
    pub t: f64,
    pub boxes: Vec<Detection>,
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

    // --- temporal / tracking fields (backward-compatible, omitted when None) ---

    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_phase: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub tracks: Option<Vec<TrackInfo>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_duration_secs: Option<f64>,

    /// Base64-encoded MP4 clip (sent on "end" phase only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clip: Option<String>,

    /// Pre-detection JPEG frames as base64 (sent on "start" phase only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_frames: Option<Vec<String>>,

    /// Per-inference bbox samples indexed into the assembled clip (sent on
    /// "end" phase only). Used by the UI to draw animated overlays synced to
    /// video playback time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clip_predictions: Option<Vec<ClipPredSample>>,
}

const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(30);

pub async fn send(
    url: &str,
    token: Option<&str>,
    payload: &WebhookPayload,
) -> Result<()> {
    // Reject plaintext HTTP when a bearer token is configured,
    // unless the target is a private/local network address (RFC1918, loopback).
    if token.is_some() && url.starts_with("http://") && !is_private_url(url).await {
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
async fn is_private_url(url: &str) -> bool {
    let host = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .and_then(|s| s.split('/').next())
        .and_then(|s| s.split(':').next())
        .unwrap_or("");
    if host.is_empty() {
        return false;
    }
    if host == "localhost" {
        return true;
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return is_private_ip(ip);
    }
    if host.ends_with(".local") {
        return true;
    }
    // Resolve DNS: accept the host only if every resolved address is private.
    // Split-horizon names that also resolve to a public IP are treated as public.
    match tokio::net::lookup_host(format!("{host}:0")).await {
        Ok(iter) => {
            let addrs: Vec<_> = iter.collect();
            !addrs.is_empty() && addrs.iter().all(|sa| is_private_ip(sa.ip()))
        }
        Err(_) => false,
    }
}

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn empty_urls_is_error() {
        assert!(parse_targets(&[], &[]).is_err());
    }

    #[test]
    fn no_tokens_means_all_none() {
        let t = parse_targets(&s(&["a", "b"]), &[]).unwrap();
        assert_eq!(t.len(), 2);
        assert!(t.iter().all(|x| x.token.is_none()));
    }

    #[test]
    fn single_token_broadcasts() {
        let t = parse_targets(&s(&["a", "b", "c"]), &s(&["TOK"])).unwrap();
        assert_eq!(t.len(), 3);
        assert!(t.iter().all(|x| x.token.as_deref() == Some("TOK")));
    }

    #[test]
    fn single_empty_token_broadcasts_none() {
        let t = parse_targets(&s(&["a", "b"]), &s(&[""])).unwrap();
        assert!(t.iter().all(|x| x.token.is_none()));
    }

    #[test]
    fn paired_tokens() {
        let t = parse_targets(&s(&["a", "b"]), &s(&["TA", "TB"])).unwrap();
        assert_eq!(t[0].token.as_deref(), Some("TA"));
        assert_eq!(t[1].token.as_deref(), Some("TB"));
    }

    #[test]
    fn paired_empty_slot_means_none() {
        let t = parse_targets(&s(&["a", "b"]), &s(&["TA", ""])).unwrap();
        assert_eq!(t[0].token.as_deref(), Some("TA"));
        assert!(t[1].token.is_none());
    }

    #[test]
    fn mismatched_count_is_error() {
        assert!(parse_targets(&s(&["a", "b", "c"]), &s(&["TA", "TB"])).is_err());
        assert!(parse_targets(&s(&["a"]), &s(&["TA", "TB"])).is_err());
    }
}
