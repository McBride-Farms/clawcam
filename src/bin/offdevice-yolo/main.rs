//! Off-device YOLO worker. Pulls one camera's RTSP feed from mediamtx,
//! runs YOLO inference, and POSTs detections to clawcam-app's webhook.
//!
//! This is the off-device counterpart to the Pi-side `clawcam` binary. It
//! reuses the project's `webhook` and `detect::yolo` modules verbatim via
//! `#[path]` so the wire format and detection semantics can't drift from
//! the Pi side. The only off-device-specific code is the RTSP pipeline
//! (sibling `pipeline.rs`) and the webhook lifecycle (sibling `debounce.rs`).
//!
//! See `systemd/clawcam-offdevice-yolo@.service` for the recommended deploy
//! shape (one systemd instance per camera, optionally lifecycled by
//! mediamtx's `runOnReady` hook on the same host).

#[path = "../../webhook/mod.rs"]
mod webhook;
#[path = "../../detect/yolo.rs"]
mod yolo;
mod debounce;
mod pipeline;

use anyhow::{Context, Result};
use clap::Parser;
use gstreamer as gst;
use gstreamer::prelude::*;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

use crate::debounce::{Emit, Lifecycle, Mode};
use crate::pipeline::{Frame, create_pipeline, grab_jpeg};
use crate::webhook::{Detection, WebhookPayload, WebhookTarget, parse_targets, send};
use crate::yolo::YoloDetector;

/// Off-device YOLO worker. Pulls a camera's RTSP feed from mediamtx and
/// posts detections to a clawcam webhook.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// RTSP URL on mediamtx (e.g. `rtsp://<mediamtx-host>:8554/<camera>`).
    #[arg(long, env = "OFFDEVICE_RTSP_URL")]
    rtsp: String,

    /// Camera name. Used as `host` in the webhook so the receiver routes
    /// events to the same camera the Pi-side monitor would.
    #[arg(long, env = "OFFDEVICE_CAMNAME")]
    camname: String,

    /// One or more webhook URLs (repeat the flag or comma-separate).
    #[arg(long = "webhook", env = "OFFDEVICE_WEBHOOK_URL", value_delimiter = ',')]
    webhooks: Vec<String>,

    /// Bearer tokens paired with --webhook (zero, one, or N — see
    /// webhook::parse_targets).
    #[arg(
        long = "webhook-token",
        env = "OFFDEVICE_WEBHOOK_TOKEN",
        value_delimiter = ','
    )]
    webhook_tokens: Vec<String>,

    /// Path to the ONNX model.
    #[arg(long, env = "OFFDEVICE_MODEL", default_value = "models/yolov8n.onnx")]
    model: String,

    /// `event` (start/end phases, default) or `continuous` (throttled bursts
    /// with no event_phase). The Android app filters notifications on
    /// `event_phase == "start"` (clawcam-android EventWatcherService.kt:34),
    /// so phaseless `continuous` events are invisible to mobile users —
    /// stick with `event` unless you only care about web/SSE consumers.
    #[arg(long, env = "OFFDEVICE_MODE", default_value = "event")]
    mode: String,

    /// Continuous-mode minimum seconds between webhooks.
    #[arg(long, env = "OFFDEVICE_COOLDOWN_S", default_value_t = 2.0)]
    cooldown_s: f64,

    /// Event-mode seconds of no detection → end phase.
    #[arg(long, env = "OFFDEVICE_IDLE_S", default_value_t = 5.0)]
    idle_s: f64,

    /// Pre-scale RGB output to this width before YOLO. yolo.rs still
    /// resizes to CLAWCAM_YOLO_INPUT_SIZE internally; this just caps the
    /// appsink frame size.
    #[arg(long, env = "OFFDEVICE_YOLO_W", default_value_t = 640)]
    yolo_w: u32,

    /// Pre-scale RGB output to this height before YOLO.
    #[arg(long, env = "OFFDEVICE_YOLO_H", default_value_t = 360)]
    yolo_h: u32,

    /// Frame-recv timeout. Exit non-zero so systemd restarts and rtspsrc
    /// reconnects.
    #[arg(long, env = "OFFDEVICE_FRAME_TIMEOUT_S", default_value_t = 30)]
    frame_timeout_s: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let targets = parse_targets(&args.webhooks, &args.webhook_tokens)?;
    info!(
        "offdevice-yolo: cam={} rtsp={} mode={} targets={}",
        args.camname,
        args.rtsp,
        args.mode,
        targets.len()
    );

    let mode = Mode::parse(&args.mode);
    let mut lifecycle = Lifecycle::new(
        mode,
        Duration::from_secs_f64(args.cooldown_s.max(0.1)),
        Duration::from_secs_f64(args.idle_s.max(0.5)),
    );

    let stream = create_pipeline(&args.rtsp, args.yolo_w, args.yolo_h)?;
    stream
        .pipeline
        .set_state(gst::State::Playing)
        .context("failed to set pipeline to Playing")?;

    let mut detector = YoloDetector::load(&args.model)?;
    info!("pipeline running; waiting for frames…");

    // Bus watcher: surface EOS / pipeline errors and exit non-zero so the
    // unit's Restart=on-failure kicks rtspsrc to reconnect.
    let bus = stream.pipeline.bus().context("no bus")?;
    let pipeline_for_bus = stream.pipeline.clone();
    tokio::task::spawn_blocking(move || {
        for msg in bus.iter_timed(gst::ClockTime::NONE) {
            use gst::MessageView::*;
            match msg.view() {
                Eos(..) => {
                    warn!("pipeline EOS — RTSP source dropped");
                    let _ = pipeline_for_bus.set_state(gst::State::Null);
                    std::process::exit(2);
                }
                Error(e) => {
                    error!(
                        "pipeline error from {:?}: {} ({:?})",
                        e.src().map(|s| s.path_string()),
                        e.error(),
                        e.debug()
                    );
                    let _ = pipeline_for_bus.set_state(gst::State::Null);
                    std::process::exit(3);
                }
                _ => {}
            }
        }
    });

    let frame_timeout = Duration::from_secs(args.frame_timeout_s.max(5));
    let host = args.camname.clone();
    let pipeline = stream.pipeline.clone();
    loop {
        let frame: Frame = match stream.frames.recv_timeout(frame_timeout) {
            Ok(f) => f,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                warn!("no frame for {}s — RTSP stalled", args.frame_timeout_s);
                std::process::exit(5);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                error!("frame channel closed; exiting for restart");
                std::process::exit(4);
            }
        };

        let detections = match detector.detect(&frame.data, frame.width, frame.height) {
            Ok(d) => d,
            Err(e) => {
                warn!("YOLO inference failed: {e}");
                continue;
            }
        };

        let emit = lifecycle.step(&detections, Instant::now());
        match emit {
            Emit::None => continue,
            Emit::Single { detections } => {
                fire(&pipeline, &targets, &host, "off_device", None, detections, None).await;
            }
            Emit::Start {
                event_id,
                detections,
            } => {
                fire(
                    &pipeline,
                    &targets,
                    &host,
                    "off_device",
                    Some((event_id, "start".to_string())),
                    detections,
                    None,
                )
                .await;
            }
            Emit::End {
                event_id,
                duration_secs,
            } => {
                fire(
                    &pipeline,
                    &targets,
                    &host,
                    "off_device",
                    Some((event_id, "end".to_string())),
                    &[],
                    Some(duration_secs),
                )
                .await;
            }
        }
    }
}

async fn fire(
    pipeline: &gst::Pipeline,
    targets: &[WebhookTarget],
    host: &str,
    detail: &str,
    event_meta: Option<(String, String)>,
    detections: &[Detection],
    duration_secs: Option<f64>,
) {
    let jpeg = match grab_jpeg(pipeline) {
        Ok(j) => j,
        Err(e) => {
            warn!("couldn't grab JPEG snapshot: {e}");
            Vec::new()
        }
    };

    let now = chrono::Utc::now();
    let (event_id, event_phase) = match event_meta {
        Some((id, phase)) => (Some(id), Some(phase)),
        None => (None, None),
    };

    let payload = WebhookPayload {
        ts: now.format("%b %d %H:%M:%S").to_string(),
        epoch: now.timestamp(),
        event_type: "motion".to_string(),
        detail: detail.to_string(),
        source: "clawcam-offdevice".to_string(),
        host: host.to_string(),
        image: base64::engine::general_purpose::STANDARD.encode(&jpeg),
        predictions: detections.to_vec(),
        event_id,
        event_phase,
        tracks: None,
        event_duration_secs: duration_secs,
        clip: None,
        pre_frames: None,
        clip_predictions: None,
    };

    let payload = std::sync::Arc::new(payload);
    let mut set = tokio::task::JoinSet::new();
    for t in targets {
        let url = t.url.clone();
        let token = t.token.clone();
        let payload = payload.clone();
        set.spawn(async move {
            let res = send(&url, token.as_deref(), &payload).await;
            (url, res)
        });
    }
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((url, Ok(()))) => tracing::debug!("webhook delivered → {url}"),
            Ok((url, Err(e))) => warn!("webhook failed for {url}: {e}"),
            Err(e) => warn!("webhook task panicked: {e}"),
        }
    }
}

// Pull base64 in directly so we don't drag the b64 helper out of monitor.rs.
use base64::Engine as _;
