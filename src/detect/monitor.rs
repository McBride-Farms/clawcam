use anyhow::{Context, Result};
use base64::Engine;
use gstreamer::prelude::*;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};
use tracing::{info, warn};

use crate::detect::pipeline;
use crate::detect::yolo::YoloDetector;
use crate::webhook::{self, WebhookPayload};

const INFERENCE_INTERVAL: Duration = Duration::from_millis(500);
const MOTION_COOLDOWN: Duration = Duration::from_secs(3);

pub async fn run_monitor(
    webhook_url: Option<&str>,
    webhook_token: Option<&str>,
    host: Option<&str>,
    log_path: Option<&str>,
) -> Result<()> {
    // Set up file logging if requested (try_init to avoid panic if already initialized)
    if let Some(path) = log_path {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .context("failed to open log file")?;
        let _ = tracing_subscriber::fmt()
            .with_writer(file)
            .with_ansi(false)
            .try_init();
    }

    // Resolve webhook URL: CLI arg takes precedence, then env var from EnvironmentFile
    let webhook_url_owned = match webhook_url {
        Some(u) => u.to_string(),
        None => std::env::var("CLAWCAM_WEBHOOK")
            .context("no webhook URL — pass --webhook or set CLAWCAM_WEBHOOK")?,
    };
    let webhook_url = webhook_url_owned.as_str();

    // Resolve webhook token: CLI arg takes precedence, then env var from EnvironmentFile
    let webhook_token_owned = match webhook_token {
        Some(t) => Some(t.to_string()),
        None => std::env::var("CLAWCAM_WEBHOOK_TOKEN").ok(),
    };
    let webhook_token: Option<&str> = webhook_token_owned.as_deref();

    let camera_source =
        std::env::var("CLAWCAM_CAMERA_SOURCE").unwrap_or_else(|_| "v4l2src".to_string());
    let model_path = std::env::var("CLAWCAM_MODEL_PATH")
        .unwrap_or_else(|_| "/usr/local/share/clawcam/yolov8n.onnx".to_string());
    let hostname = host
        .map(String::from)
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "unknown".to_string());

    // Create a restricted runtime directory for frame data.
    // Try /run/clawcam first, fall back to XDG_RUNTIME_DIR, then /tmp.
    let frame_dir = ["/run/clawcam"]
        .iter()
        .map(std::path::PathBuf::from)
        .chain(dirs::runtime_dir().map(|d| d.join("clawcam")))
        .find(|d| {
            std::fs::create_dir_all(d).is_ok()
                && std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700)).is_ok()
        })
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    let latest_frame_path = frame_dir.join("clawcam_latest.jpg");

    info!("starting monitor: camera={camera_source} model={model_path}");

    // Load YOLO model
    let mut detector = YoloDetector::load(&model_path)?;
    info!("YOLO model loaded");

    // Start GStreamer pipeline
    let (frame_rx, gst_pipeline) = pipeline::create_pipeline(&camera_source, 1280, 720, 10)?;
    gst_pipeline.set_state(gstreamer::State::Playing)?;
    info!("pipeline started");

    let mut last_event = Instant::now() - MOTION_COOLDOWN;

    loop {
        // Wait for a frame
        let frame = match frame_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(f) => f,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                warn!("no frame received in 5s, pipeline may be stalled");
                continue;
            }
            Err(_) => break,
        };

        // Run inference
        let detections = match detector.detect(&frame.data, frame.width, frame.height) {
            Ok(d) => d,
            Err(e) => {
                warn!("inference failed: {e}");
                continue;
            }
        };

        // Always write latest frame so _snap can read it without opening the camera
        if let Ok(jpeg) = pipeline::grab_jpeg(&gst_pipeline) {
            if std::fs::write(&latest_frame_path, &jpeg).is_ok() {
                // Ensure the file is only readable by the owning user
                std::fs::set_permissions(&latest_frame_path, std::fs::Permissions::from_mode(0o600)).ok();
            }
        }

        // If we got detections and cooldown has elapsed, fire webhook
        if !detections.is_empty() && last_event.elapsed() >= MOTION_COOLDOWN {
            last_event = Instant::now();

            info!(
                "detected: {}",
                detections
                    .iter()
                    .map(|d| format!("{}({:.0}%)", d.class, d.score * 100.0))
                    .collect::<Vec<_>>()
                    .join(", ")
            );

            let jpeg = pipeline::grab_jpeg(&gst_pipeline).unwrap_or_default();
            let image_b64 = base64::engine::general_purpose::STANDARD.encode(&jpeg);

            let now = chrono::Utc::now();
            let payload = WebhookPayload {
                ts: now.format("%b %d %H:%M:%S").to_string(),
                epoch: now.timestamp(),
                event_type: "motion".to_string(),
                detail: "ai_detected".to_string(),
                source: "clawcam".to_string(),
                host: hostname.clone(),
                image: image_b64,
                predictions: detections,
            };

            let url = webhook_url.to_string();
            let token = webhook_token.map(String::from);
            tokio::spawn(async move {
                if let Err(e) = webhook::send(&url, token.as_deref(), &payload).await {
                    warn!("webhook send failed: {e}");
                }
            });
        }

        // Pace inference
        tokio::time::sleep(INFERENCE_INTERVAL).await;
    }

    gst_pipeline.set_state(gstreamer::State::Null)?;
    info!("monitor stopped");
    Ok(())
}
