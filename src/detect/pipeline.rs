use anyhow::{Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use std::sync::mpsc;

/// A frame captured from the GStreamer pipeline.
pub struct Frame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Build a GStreamer pipeline that captures frames for inference.
///
/// Pipeline layout:
///   source → convert → scale → capsfilter → tee
///     tee → queue → jpegenc → jpeg_sink (for snapshots / webhook images)
///     tee → queue → convert → capsfilter(RGB) → rgb_sink (for YOLO inference)
///
/// Returns a receiver that yields RGB frames and the pipeline handle.
pub fn create_pipeline(
    source_name: &str,
    width: u32,
    height: u32,
    fps: u32,
) -> Result<(mpsc::Receiver<Frame>, gst::Pipeline)> {
    gst::init().context("failed to initialize GStreamer")?;

    let pipeline = gst::Pipeline::default();

    // Source
    let source = gst::ElementFactory::make(source_name)
        .build()
        .context(format!("failed to create {source_name}"))?;

    // Common path: convert → scale → caps → tee
    let convert = gst::ElementFactory::make("videoconvert").build()?;
    let scale = gst::ElementFactory::make("videoscale").build()?;
    let caps = gst::ElementFactory::make("capsfilter")
        .property(
            "caps",
            gst::Caps::builder("video/x-raw")
                .field("width", width as i32)
                .field("height", height as i32)
                .field("framerate", gst::Fraction::new(fps as i32, 1))
                .build(),
        )
        .build()?;
    let tee = gst::ElementFactory::make("tee").build()?;

    // JPEG branch: queue → jpegenc → appsink
    let jpeg_queue = gst::ElementFactory::make("queue").build()?;
    let jpegenc = gst::ElementFactory::make("jpegenc")
        .property("quality", 85i32)
        .build()?;
    let jpeg_sink = gst_app::AppSink::builder()
        .name("jpeg_sink")
        .max_buffers(2)
        .drop(true)
        .build();

    // RGB branch: queue → videoconvert → capsfilter(RGB) → appsink
    let rgb_queue = gst::ElementFactory::make("queue").build()?;
    let rgb_convert = gst::ElementFactory::make("videoconvert").build()?;
    let rgb_caps = gst::ElementFactory::make("capsfilter")
        .property(
            "caps",
            gst::Caps::builder("video/x-raw")
                .field("format", "RGB")
                .build(),
        )
        .build()?;
    let rgb_sink = gst_app::AppSink::builder()
        .name("rgb_sink")
        .max_buffers(2)
        .drop(true)
        .build();

    // Add all elements
    pipeline.add_many([
        &source, &convert, &scale, &caps, &tee,
        &jpeg_queue, &jpegenc, jpeg_sink.upcast_ref(),
        &rgb_queue, &rgb_convert, &rgb_caps, rgb_sink.upcast_ref(),
    ])?;

    // Link common path
    gst::Element::link_many([&source, &convert, &scale, &caps, &tee])?;

    // Link JPEG branch
    gst::Element::link_many([&jpeg_queue, &jpegenc, jpeg_sink.upcast_ref()])?;
    tee.link_pads(None, &jpeg_queue, None)?;

    // Link RGB branch
    gst::Element::link_many([&rgb_queue, &rgb_convert, &rgb_caps, rgb_sink.upcast_ref()])?;
    tee.link_pads(None, &rgb_queue, None)?;

    // Set up callback for RGB frames
    let (tx, rx) = mpsc::sync_channel::<Frame>(4);
    let w = width;
    let h = height;
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

    Ok((rx, pipeline))
}

/// Grab a single JPEG from the pipeline's jpeg_sink.
pub fn grab_jpeg(pipeline: &gst::Pipeline) -> Result<Vec<u8>> {
    let jpeg_sink = pipeline
        .by_name("jpeg_sink")
        .context("jpeg_sink not found")?
        .downcast::<gst_app::AppSink>()
        .map_err(|_| anyhow::anyhow!("jpeg_sink cast failed"))?;

    let sample = jpeg_sink
        .pull_sample()
        .map_err(|_| anyhow::anyhow!("failed to pull JPEG sample"))?;
    let buffer = sample.buffer().context("no buffer in sample")?;
    let map = buffer.map_readable()?;
    Ok(map.to_vec())
}
