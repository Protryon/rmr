use std::{path::PathBuf, process::Stdio};

use image::RgbImage;
use log::info;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::Command,
    sync::mpsc,
};
use url::Url;

pub struct FFmpegConfig {
    pub binary: String,
    pub rtsp_input: Url,
    pub record_single_jpeg: bool,
    pub recording_mp4_dir: Option<PathBuf>,
    pub send_images: Option<mpsc::Sender<RgbImage>>,
    pub image_width: Option<u32>,
    pub image_height: Option<u32>,
    pub force_tcp: bool,
}

#[derive(Debug, Error)]
pub enum FFMpegError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("probe parse failed: {0}")]
    ProbeParse(serde_json::Error),
    #[error("no video stream in RTSP")]
    NoVideoStream,
    #[error("expected video codec h265/hevc, got {0}")]
    UnsupportedVideoCodec(String),
    #[error("ffmpeg exited with code {0}")]
    ExitedWithError(i32),
    #[error("error reading next image from ffmpeg: {0}")]
    ErrorReadingImage(std::io::Error),
}

#[derive(Serialize, Deserialize)]
struct FFProbeStreams {
    streams: Vec<FFProbeStream>,
}

#[derive(Serialize, Deserialize)]
struct FFProbeStream {
    index: usize,
    codec_name: String,
    codec_long_name: String,
    codec_tag_string: String,
    codec_tag: String,
    #[serde(flatten)]
    data: FFProbeStreamData,
    r_frame_rate: String,
    avg_frame_rate: String,
    time_base: String,
    // start_pts: u64,
    // start_time: String,
}

#[derive(Serialize, Deserialize)]
struct FFProbeVideoStreamData {
    width: u32,
    height: u32,
    coded_width: u32,
    coded_height: u32,
    closed_captions: u32,
    has_b_frames: u32,
    pix_fmt: String,
    level: u32,
    color_range: String,
    color_space: String,
    color_transfer: String,
    color_primaries: String,
    chroma_location: String,
    // field_order: String,
    refs: u32,
    is_avc: String,
    nal_length_size: String,
    bits_per_raw_sample: String,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "codec_type", rename_all = "snake_case")]
enum FFProbeStreamData {
    Video(FFProbeVideoStreamData),
    Audio {
        sample_fmt: String,
        sample_rate: String,
        channels: u32,
        bits_per_sample: u32,
        bit_rate: String,
    },
}

impl FFmpegConfig {
    pub async fn run(&self) -> Result<(), FFMpegError> {
        let ffprobe = self.binary.replace("ffmpeg", "ffprobe");
        info!("Running '{ffprobe}' as ffprobe binary");
        let ffprobe_out = Command::new(&ffprobe)
            .arg(&self.rtsp_input.as_str())
            .args(["-rtsp_transport", "tcp", "-of", "json", "-show_streams"])
            .output()
            .await?;
        let ffprobe_out: FFProbeStreams =
            serde_json::from_slice(&ffprobe_out.stdout).map_err(FFMpegError::ProbeParse)?;
        let video_stream = ffprobe_out
            .streams
            .iter()
            .find(|x| matches!(x.data, FFProbeStreamData::Video(_)))
            .ok_or(FFMpegError::NoVideoStream)?;
        // let audio_stream = ffprobe_out.streams.iter().find(|x| matches!(x.data, FFProbeStreamData::Audio { .. }));

        let video_codec = &video_stream.codec_name;
        if video_codec != "h264" && video_codec != "hevc" {
            return Err(FFMpegError::UnsupportedVideoCodec(video_codec.clone()));
        }

        info!("ffprobe complete, beginning stream");

        let FFProbeStreamData::Video(video_data) = &video_stream.data else {
            unreachable!();
        };

        let width_out = self.image_width.unwrap_or(video_data.width);
        let height_out = self.image_height.unwrap_or(video_data.height);
        let dimension = format!("{}x{}", width_out, height_out);

        let mut ffmpeg_args = vec![];
        if self.force_tcp {
            ffmpeg_args.extend(["-rtsp_transport", "tcp"]);
        }
        let mut recording_format = self.recording_mp4_dir.clone();
        if let Some(recording_format) = &mut recording_format {
            if self.record_single_jpeg {
                recording_format.push("screenshot.jpg");
                ffmpeg_args.extend([
                    "-s",
                    &dimension,
                    "-frames:v",
                    "1",
                    "-y",
                    recording_format.to_str().unwrap(),
                ]);
            } else {
                recording_format.push("%Y%m%d-%H%M%S%z.mp4");
                ffmpeg_args.extend([
                    "-c:v",
                    "copy",
                    "-segment_time",
                    "00:1:00",
                    "-f",
                    "segment",
                    "-reset_timestamps",
                    "1",
                    "-segment_atclocktime",
                    "1",
                    "-strftime",
                    "1",
                    "-codec:a",
                    "aac",
                    "-y",
                    recording_format.to_str().unwrap(),
                ]);
            }
        }
        if self.send_images.is_some() {
            ffmpeg_args.extend(["-f", "rawvideo", "-pix_fmt", "rgb24", "-s", &dimension, "-"])
        }

        info!(
            "ffmpeg: {} -i {} {}",
            self.binary,
            self.rtsp_input,
            ffmpeg_args.join(" ")
        );
        let mut ffmpeg_process = Command::new(&self.binary)
            .arg("-i")
            .arg(&self.rtsp_input.as_ref())
            .args(&ffmpeg_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stderr = ffmpeg_process.stderr.take().unwrap();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                eprintln!("{line}");
            }
        });

        let mut stdout = ffmpeg_process.stdout.take().unwrap();
        let image_size = width_out * height_out * 3;
        if let Some(send_images) = &self.send_images {
            let mut image_buf = vec![0u8; image_size as usize];
            loop {
                if let Err(e) = stdout.read_exact(&mut image_buf).await {
                    return Err(FFMpegError::ErrorReadingImage(e));
                }
                if send_images
                    .send(RgbImage::from_raw(width_out, height_out, image_buf.clone()).unwrap())
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }

        let status = ffmpeg_process.wait().await?;
        if !status.success() {
            return Err(FFMpegError::ExitedWithError(
                status.code().unwrap_or_default(),
            ));
        }
        Ok(())
    }
}
