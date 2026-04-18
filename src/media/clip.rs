use anyhow::{Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;

use crate::device::Device;
use crate::ssh::session;
use crate::media::detect_source;

/// Remote clip: SSHes into the device and runs `clawcam _clip` there.
pub async fn run_clip(dev: &Device, duration: u32, out: Option<&str>) -> Result<()> {
    let remote_path = "/tmp/clawcam_clip.mp4";

    session::run_cmd(dev, &format!(
        "clawcam _clip --dur {duration} --out {remote_path}"
    )).await.context("clip failed on device — is clawcam installed?")?;

    let local_path = out.unwrap_or("clip.mp4");
    session::scp_from(dev, remote_path, local_path).await?;
    session::run_cmd(dev, &format!("rm -f {remote_path}")).await?;

    println!("clip saved to {local_path} ({duration}s)");
    Ok(())
}

/// Detect the best available H.264 encoder via GStreamer.
fn detect_encoder() -> &'static str {
    if gst::ElementFactory::find("v4l2h264enc").is_some() {
        "v4l2h264enc"
    } else {
        "x264enc"
    }
}

/// On-device clip: uses GStreamer Rust API to record an MP4.
pub fn run_clip_local(duration: u32, out: &str) -> Result<()> {
    gst::init().context("failed to initialize GStreamer")?;

    let source_name = detect_source();
    let encoder_name = detect_encoder();
    let pipeline = gst::Pipeline::default();

    let source = gst::ElementFactory::make(&source_name)
        .build()
        .context(format!("failed to create {source_name}"))?;

    let convert = gst::ElementFactory::make("videoconvert")
        .build()
        .context("failed to create videoconvert")?;

    let capsfilter = gst::ElementFactory::make("capsfilter")
        .property(
            "caps",
            gst::Caps::builder("video/x-raw")
                .field("width", 1920i32)
                .field("height", 1080i32)
                .field("framerate", gst::Fraction::new(30, 1))
                .build(),
        )
        .build()
        .context("failed to create capsfilter")?;

    let encoder = gst::ElementFactory::make(encoder_name)
        .build()
        .context(format!("failed to create {encoder_name}"))?;

    // Set x264enc-specific properties
    if encoder_name == "x264enc" {
        encoder.set_property_from_str("tune", "zerolatency");
        encoder.set_property("bitrate", 2000u32);
    }

    let parser = gst::ElementFactory::make("h264parse")
        .build()
        .context("failed to create h264parse")?;

    let muxer = gst::ElementFactory::make("mp4mux")
        .build()
        .context("failed to create mp4mux")?;

    let sink = gst::ElementFactory::make("filesink")
        .property("location", out)
        .build()
        .context("failed to create filesink")?;

    pipeline.add_many([&source, &convert, &capsfilter, &encoder, &parser, &muxer, &sink])?;
    gst::Element::link_many([&source, &convert, &capsfilter, &encoder, &parser, &muxer, &sink])?;

    pipeline.set_state(gst::State::Playing)?;

    std::thread::sleep(std::time::Duration::from_secs(duration as u64));

    pipeline.send_event(gst::event::Eos::new());

    let bus = pipeline.bus().context("no bus")?;
    bus.timed_pop_filtered(
        gst::ClockTime::from_seconds(5),
        &[gst::MessageType::Eos, gst::MessageType::Error],
    );

    pipeline.set_state(gst::State::Null)?;
    println!("{out}");
    Ok(())
}
