//! RTSP-pull GStreamer pipeline for the off-device YOLO worker.
//!
//! Off-device analogue of `crate::detect::pipeline` (Pi side): same
//! tee/JPEG/RGB shape, but the source is mediamtx RTSP instead of
//! `v4l2src`/`libcamerasrc`. We don't republish a stream — the Pi already
//! pushed into mediamtx, and mediamtx fans it out to whoever wants it.
//!
//!   rtspsrc(tcp) → rtph264depay → h264parse → avdec_h264 → videoconvert → tee
//!     tee → queue → jpegenc → appsink (jpeg_sink)
//!     tee → queue(leaky) → videoscale → videoconvert → caps(RGB,WxH) → appsink (rgb_sink)
//!
//! `rtspsrc` exposes RTP src pads only after SDP/PLAY completes, so the
//! `rtspsrc → depay` link is set up in a `pad-added` callback rather than
//! linked statically.

use anyhow::{Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use std::sync::mpsc;

pub struct Frame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

pub struct StreamPipeline {
    pub pipeline: gst::Pipeline,
    pub frames: mpsc::Receiver<Frame>,
}

pub fn create_pipeline(rtsp_url: &str, yolo_w: u32, yolo_h: u32) -> Result<StreamPipeline> {
    gst::init().context("failed to initialize GStreamer")?;
    let pipeline = gst::Pipeline::default();

    // Force TCP transport. UDP packet loss across the LAN shows up as
    // glitchy frames YOLO interprets as confident garbage.
    let latency_ms: u32 = std::env::var("OFFDEVICE_RTSP_LATENCY_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);
    let source = gst::ElementFactory::make("rtspsrc")
        .property("location", rtsp_url)
        .property("latency", latency_ms)
        .property_from_str("protocols", "tcp")
        .property("do-retransmission", false)
        .build()
        .context("rtspsrc unavailable — install gstreamer1.0-rtsp")?;

    let depay = gst::ElementFactory::make("rtph264depay").build()?;
    let parse = gst::ElementFactory::make("h264parse").build()?;
    let decoder = gst::ElementFactory::make("avdec_h264")
        .build()
        .context("avdec_h264 unavailable — install gstreamer1.0-libav")?;
    let convert = gst::ElementFactory::make("videoconvert").build()?;
    let tee = gst::ElementFactory::make("tee")
        .property("allow-not-linked", true)
        .build()?;

    let jpeg_queue = gst::ElementFactory::make("queue")
        .property_from_str("leaky", "downstream")
        .property("max-size-buffers", 2u32)
        .build()?;
    let jpeg_convert = gst::ElementFactory::make("videoconvert").build()?;
    let jpegenc = gst::ElementFactory::make("jpegenc")
        .property("quality", 85i32)
        .build()?;
    let jpeg_sink = gst_app::AppSink::builder()
        .name("jpeg_sink")
        .max_buffers(2)
        .drop(true)
        .build();

    let rgb_queue = gst::ElementFactory::make("queue")
        .property_from_str("leaky", "downstream")
        .property("max-size-buffers", 1u32)
        .property("max-size-bytes", 0u32)
        .property("max-size-time", 0u64)
        .build()?;
    let rgb_scale = gst::ElementFactory::make("videoscale").build()?;
    let rgb_convert = gst::ElementFactory::make("videoconvert").build()?;
    let rgb_caps = gst::ElementFactory::make("capsfilter")
        .property(
            "caps",
            gst::Caps::builder("video/x-raw")
                .field("format", "RGB")
                .field("width", yolo_w as i32)
                .field("height", yolo_h as i32)
                .build(),
        )
        .build()?;
    let rgb_sink = gst_app::AppSink::builder()
        .name("rgb_sink")
        .max_buffers(2)
        .drop(true)
        .build();

    pipeline.add_many([
        &source,
        &depay,
        &parse,
        &decoder,
        &convert,
        &tee,
        &jpeg_queue,
        &jpeg_convert,
        &jpegenc,
        jpeg_sink.upcast_ref(),
        &rgb_queue,
        &rgb_scale,
        &rgb_convert,
        &rgb_caps,
        rgb_sink.upcast_ref(),
    ])?;

    gst::Element::link_many([&depay, &parse, &decoder, &convert, &tee])?;
    gst::Element::link_many([
        &jpeg_queue,
        &jpeg_convert,
        &jpegenc,
        jpeg_sink.upcast_ref(),
    ])?;
    tee.link_pads(None, &jpeg_queue, None)?;
    gst::Element::link_many([
        &rgb_queue,
        &rgb_scale,
        &rgb_convert,
        &rgb_caps,
        rgb_sink.upcast_ref(),
    ])?;
    tee.link_pads(None, &rgb_queue, None)?;

    // Dynamic-pad bridge: rtspsrc's RTP video src pad shows up post-PLAY.
    // Filter for H.264 video; ignore audio/metadata silently.
    let depay_sink = depay.static_pad("sink").context("depay sink pad")?;
    source.connect_pad_added(move |_src, pad| {
        if depay_sink.is_linked() {
            return;
        }
        let caps = match pad.current_caps() {
            Some(c) => c,
            None => return,
        };
        let structure = match caps.structure(0) {
            Some(s) => s,
            None => return,
        };
        let media = structure.get::<&str>("media").unwrap_or("");
        let encoding = structure.get::<&str>("encoding-name").unwrap_or("");
        if media == "video" && encoding.eq_ignore_ascii_case("H264") {
            match pad.link(&depay_sink) {
                Ok(_) => tracing::info!("rtspsrc → depay linked (encoding={encoding})"),
                Err(e) => tracing::warn!("rtspsrc → depay link failed: {e:?}"),
            }
        } else {
            tracing::debug!("ignoring rtsp pad media={media} encoding={encoding}");
        }
    });

    let (tx, rx) = mpsc::sync_channel::<Frame>(4);
    let w = yolo_w;
    let h = yolo_h;
    rgb_sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Error)?;
                let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                let _ = tx.try_send(Frame {
                    data: map.to_vec(),
                    width: w,
                    height: h,
                });
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );

    Ok(StreamPipeline { pipeline, frames: rx })
}

pub fn grab_jpeg(pipeline: &gst::Pipeline) -> Result<Vec<u8>> {
    let jpeg_sink = pipeline
        .by_name("jpeg_sink")
        .context("jpeg_sink not found")?
        .downcast::<gst_app::AppSink>()
        .map_err(|_| anyhow::anyhow!("jpeg_sink cast failed"))?;
    let sample = jpeg_sink
        .try_pull_sample(gst::ClockTime::from_mseconds(500))
        .ok_or_else(|| anyhow::anyhow!("no JPEG sample within 500ms"))?;
    let buffer = sample.buffer().context("no buffer in sample")?;
    let map = buffer.map_readable()?;
    Ok(map.to_vec())
}
