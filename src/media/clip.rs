use anyhow::{Context, Result};

use crate::device::Device;
use crate::ssh::session;

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

/// On-device clip: uses GStreamer Rust API to record an MP4.
pub fn run_clip_local(duration: u32, out: &str) -> Result<()> {
    use gstreamer as gst;
    use gstreamer::prelude::*;

    gst::init().context("failed to initialize GStreamer")?;

    let source = std::env::var("CLAWCAM_CAMERA_SOURCE")
        .unwrap_or_else(|_| "v4l2src".to_string());

    let pipeline = gst::parse::launch(&format!(
        "{source} ! videoconvert ! video/x-raw,width=1280,height=720,framerate=30/1 ! \
         x264enc tune=zerolatency bitrate=2000 ! h264parse ! \
         mp4mux ! filesink location={out}"
    ))
    .context("failed to create clip pipeline")?
    .downcast::<gst::Pipeline>()
    .map_err(|_| anyhow::anyhow!("pipeline cast failed"))?;

    pipeline.set_state(gst::State::Playing)?;

    // Record for the specified duration
    std::thread::sleep(std::time::Duration::from_secs(duration as u64));

    // Send EOS to finalize the MP4
    pipeline.send_event(gst::event::Eos::new());

    // Wait for EOS to propagate
    let bus = pipeline.bus().context("no bus")?;
    bus.timed_pop_filtered(
        gst::ClockTime::from_seconds(5),
        &[gst::MessageType::Eos, gst::MessageType::Error],
    );

    pipeline.set_state(gst::State::Null)?;
    println!("{out}");
    Ok(())
}
