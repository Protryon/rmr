use std::{path::Path, process::Stdio};

use crate::{config::CONFIG, modect::MotionDetectionEvent};
use anyhow::{bail, Context, Result};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};

pub async fn modect_mp4(
    event: &MotionDetectionEvent,
    frame_rate: u32,
    destination: &Path,
) -> Result<()> {
    let first_frame = event
        .frames
        .first()
        .context("missing single frame for event")?;
    let frame_rate = frame_rate.to_string();
    let dimension = format!(
        "{}x{}",
        first_frame.image.width(),
        first_frame.image.height()
    );
    let mut process = Command::new(&CONFIG.ffmpeg_bin)
        .args(&[
            "-f",
            "rawvideo",
            "-pixel_format",
            "rgb24",
            "-video_size",
            &dimension,
            "-framerate",
            &frame_rate,
            "-c:v",
            "h264",
            "-flags",
            "+cgop",
            "-",
        ])
        .arg(destination)
        .stderr(Stdio::piped())
        .spawn()?;

    let stderr = process.stderr.take().unwrap();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            eprintln!("{line}");
        }
    });

    let status = process.wait().await?;
    if !status.success() {
        bail!("ffmpeg failed with code: {status}");
    }

    Ok(())
}
