use anyhow::{Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use crate::device::Device;
use crate::ssh::session;
use crate::media::detect_source;

const LATEST_FRAME: &str = "/tmp/clawcam_latest.jpg";

/// Remote snap: SSHes into the device and runs `clawcam _snap` there.
pub async fn run_snap(dev: &Device, out: Option<&str>) -> Result<()> {
    let remote_path = "/tmp/clawcam_snap.jpg";

    session::run_cmd(dev, &format!(
        "clawcam _snap --out {remote_path}"
    )).await.context("snap failed on device — is clawcam installed?")?;

    let local_path = out.unwrap_or("snapshot.jpg");
    session::scp_from(dev, remote_path, local_path).await?;
    session::run_cmd(dev, &format!("rm -f {remote_path}")).await?;

    println!("snapshot saved to {local_path}");
    Ok(())
}

/// On-device snap: if the monitor is running, read its latest frame.
/// Otherwise, open the camera directly.
pub fn run_snap_local(out: &str) -> Result<()> {
    let latest = std::path::Path::new(LATEST_FRAME);
    if latest.exists() {
        let metadata = std::fs::metadata(latest)?;
        let age = metadata.modified()?.elapsed().unwrap_or_default();
        if age.as_secs() < 10 {
            std::fs::copy(latest, out)?;
            println!("{out}");
            return Ok(());
        }
    }

    capture_fresh(out)
}

/// Open the camera via GStreamer Rust API and capture a single JPEG frame.
fn capture_fresh(out: &str) -> Result<()> {
    gst::init().context("failed to initialize GStreamer")?;

    let source_name = detect_source();
    let pipeline = gst::Pipeline::default();

    let source = gst::ElementFactory::make(&source_name)
        .build()
        .context(format!("failed to create {source_name} element"))?;

    let convert = gst::ElementFactory::make("videoconvert")
        .build()
        .context("failed to create videoconvert")?;

    let scale = gst::ElementFactory::make("videoscale")
        .build()
        .context("failed to create videoscale")?;

    let capsfilter = gst::ElementFactory::make("capsfilter")
        .property(
            "caps",
            gst::Caps::builder("video/x-raw")
                .field("width", 1920i32)
                .field("height", 1080i32)
                .build(),
        )
        .build()
        .context("failed to create capsfilter")?;

    let encoder = gst::ElementFactory::make("jpegenc")
        .property("quality", 90i32)
        .build()
        .context("failed to create jpegenc")?;

    let sink = gst_app::AppSink::builder()
        .max_buffers(1)
        .drop(true)
        .build();

    pipeline.add_many([&source, &convert, &scale, &capsfilter, &encoder, sink.upcast_ref()])?;
    gst::Element::link_many([&source, &convert, &scale, &capsfilter, &encoder, sink.upcast_ref()])?;

    pipeline.set_state(gst::State::Playing)?;

    let sample = sink
        .pull_sample()
        .map_err(|_| anyhow::anyhow!("failed to capture frame — check camera connection"))?;
    let buffer = sample.buffer().context("no buffer")?;
    let map = buffer.map_readable()?;

    std::fs::write(out, map.as_slice())?;

    pipeline.set_state(gst::State::Null)?;
    println!("{out}");
    Ok(())
}
