use std::{cmp::Ordering, io::Cursor, sync::Arc, time::Duration};

use crate::config::{PreviewFormat, PushoverPriority, CONFIG};
use crate::modect::MotionDetectionEvent;
use chrono::{DateTime, Utc};
use image::{
    codecs::gif::{GifEncoder, Repeat},
    Delay, DynamicImage, Frame, ImageFormat, RgbaImage,
};
use log::{error, info};
use reqwest::{
    multipart::{Form, Part},
    Client,
};
use serde::{Deserialize, Serialize};
use serde_with::{base64::Base64, serde_as};
use webp_animation::{Encoder, EncoderOptions, EncodingConfig, EncodingType, LossyEncodingConfig};

use crate::observable_buf::ObservableBuf;

lazy_static::lazy_static! {
    static ref CLIENT: Client = Client::new();
}

#[serde_as]
#[derive(Serialize, Deserialize, Default)]
pub struct PushoverAlert {
    pub user: String,
    pub token: String,
    pub message: String,
    #[serde_as(as = "Base64")]
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "attachment_base64")]
    pub attachment: Vec<u8>,
    #[serde(skip)]
    pub filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachment_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

const MAX_ALERT_ATTACHMENT_SIZE: usize = (1024 * 1024 * 5) / 2;
#[allow(dead_code)]
const MAX_WEBP_BYTES_PER_FRAME: usize = 8192;
const TARGET_WEBP_BYTES_PER_FRAME: usize = 7000;
const MAX_WEBP_FRAMES: usize = MAX_ALERT_ATTACHMENT_SIZE / MAX_WEBP_BYTES_PER_FRAME;

impl PushoverAlert {
    pub fn new() -> Self {
        match &CONFIG.pushover {
            None => Default::default(),
            Some(config) => PushoverAlert {
                token: config.token.clone(),
                user: config.user_key.clone(),
                priority: Some(config.priority as i32),
                ..Default::default()
            },
        }
    }

    pub async fn push(&self) {
        let Some(pushover) = &CONFIG.pushover else {
            return;
        };
        let mut body = Form::new()
            .text("user", self.user.clone())
            .text("token", self.token.clone())
            .text("message", self.message.clone())
            .text("html", "1");
        if let Some(attachment_type) = &self.attachment_type {
            body = body.text("attachment_type", attachment_type.clone());
        }
        if let Some(priority) = &self.priority {
            body = body.text("priority", priority.to_string());
        }
        if let Some(timestamp) = &self.timestamp {
            body = body.text("timestamp", timestamp.to_string());
        }
        if let Some(title) = &self.title {
            body = body.text("title", title.to_string());
        }
        if !self.attachment.is_empty() {
            let mut part = Part::bytes(self.attachment.clone());
            if let Some(filename) = &self.filename {
                part = part.file_name(filename.clone());
            }
            if let Some(attachment_type) = &self.attachment_type {
                part = part.mime_str(attachment_type).unwrap();
            }
            body = body.part("attachment", part);
        }
        match CLIENT
            .post(pushover.url.clone())
            .multipart(body)
            .send()
            .await
        {
            Ok(response) => {
                if !response.status().is_success() {
                    error!(
                        "failed to send alert: HTTP status {}:\n{}",
                        response.status(),
                        response.text().await.unwrap_or_default()
                    );
                }
            }
            Err(e) => {
                error!("failed to send alert: {e}");
            }
        }
    }
}

pub enum AlertState {
    Confirmed,
    Completed,
    CompletedAfterConfirm,
}

fn attach_jpeg(alert: &mut PushoverAlert, event: &MotionDetectionEvent) {
    if let Some(best_frame) = event
        .frames
        .iter()
        .max_by(|x, y| x.change.partial_cmp(&y.change).unwrap_or(Ordering::Less))
    {
        alert.attachment_type = Some("image/jpeg".to_string());
        alert.filename = Some("event.jpeg".to_string());
        let mut cursor = Cursor::new(&mut alert.attachment);
        best_frame
            .image
            .write_to(&mut cursor, ImageFormat::Jpeg)
            .expect("failed to encode jpeg");
    }
}

fn attach_gif(alert: &mut PushoverAlert, event: &MotionDetectionEvent, frame_rate: f64) {
    let (buf, len_ref) = ObservableBuf::new(&mut alert.attachment);
    let mut encoder = GifEncoder::new(buf);
    encoder.set_repeat(Repeat::Infinite).unwrap();
    let mut acceptable_ending = 0usize;
    for frame in &event.frames {
        let image: RgbaImage = DynamicImage::ImageRgb8(frame.image.clone()).to_rgba8();
        encoder
            .encode_frame(Frame::from_parts(
                image,
                0,
                0,
                Delay::from_saturating_duration(Duration::from_secs_f64(1.0 / frame_rate)),
            ))
            .unwrap();
        let len = len_ref.load(std::sync::atomic::Ordering::SeqCst);
        if len > MAX_ALERT_ATTACHMENT_SIZE {
            break;
        }
        acceptable_ending = len;
    }
    drop(encoder);
    alert.attachment.truncate(acceptable_ending);
    // tokio::fs::write("./test.gif", &alert.attachment).await.unwrap();
    alert.attachment_type = Some("image/gif".to_string());
    alert.filename = Some("event.gif".to_string());
}

async fn attach_webp(
    alert: &mut PushoverAlert,
    event: &Arc<MotionDetectionEvent>,
    frame_rate: f64,
) {
    let event = event.clone();
    alert.attachment = tokio::task::spawn_blocking(move || {
        let mut encoder = Encoder::new_with_options(
            event.frames.first().unwrap().image.dimensions(),
            EncoderOptions {
                minimize_size: true,
                encoding_config: Some(EncodingConfig {
                    encoding_type: EncodingType::Lossy(LossyEncodingConfig {
                        target_size: TARGET_WEBP_BYTES_PER_FRAME / 2,
                        ..LossyEncodingConfig::new_from_picture_preset()
                    }),
                    quality: 25.0,
                    method: 3,
                }),
                ..Default::default()
            },
        )
        .unwrap();

        let ms_per_frame = 1000 / frame_rate as i32;
        let mut frame_index = 0f64;
        let mut last_frame_index = -1isize;
        let mut encoded_frames = 0usize;
        for encoded_frame_index in 0..MAX_WEBP_FRAMES {
            let mut target_index = frame_index.round() as usize;
            if target_index <= last_frame_index as usize {
                target_index = last_frame_index as usize + 1;
            }

            let Some(frame) = event.frames.get(target_index) else {
                break;
            };

            let image: RgbaImage = DynamicImage::ImageRgb8(frame.image.clone()).to_rgba8();
            encoder
                .add_frame(image.as_raw(), ms_per_frame * encoded_frame_index as i32)
                .unwrap();

            last_frame_index = target_index as isize;
            frame_index += event.frames.len() as f64 / MAX_WEBP_FRAMES as f64;
            encoded_frames += 1;
        }
        encoder
            .finalize(encoded_frames as i32 * ms_per_frame)
            .unwrap()
            .to_vec()
    })
    .await
    .unwrap();
    tokio::fs::write("./test.webp", &alert.attachment)
        .await
        .unwrap();
    //todo: ???
    if alert.attachment.len() > MAX_ALERT_ATTACHMENT_SIZE {
        error!(
            "webp encoded too large! was {} bytes, expected <= {MAX_ALERT_ATTACHMENT_SIZE}",
            alert.attachment.len()
        );
        return;
    }
    alert.attachment_type = Some("image/webp".to_string());
    alert.filename = Some("event.webp".to_string());
}

pub async fn alert_event(
    time: DateTime<Utc>,
    event: Arc<MotionDetectionEvent>,
    camera_alert_priority: Option<PushoverPriority>,
    camera_name: &str,
    frame_rate: f64,
    state: AlertState,
) {
    let mut alert = PushoverAlert::new();
    if let Some(priority) = camera_alert_priority {
        alert.priority = Some(priority as i32);
    }
    if alert.priority == Some(PushoverPriority::Ignore as i32) {
        return;
    }
    alert.timestamp = Some(time.timestamp() as u64);
    alert.title = Some(match state {
        AlertState::Confirmed => format!("Ongoing Motion @ {camera_name}"),
        AlertState::Completed => format!("Motion @ {camera_name}"),
        AlertState::CompletedAfterConfirm => {
            alert.priority = Some(PushoverPriority::Lowest as i32);
            format!("Ended Ongoing Motion @ {camera_name}")
        }
    });
    alert.message = format!(
        r"Total Score: {:.02}<br> Start Frame: {}<br>Total Frames: {}",
        event.total_score,
        event.start_stream_frame_number,
        event.end_stream_frame_number - event.start_stream_frame_number
    );

    match CONFIG
        .pushover
        .as_ref()
        .map(|x| x.preview_format)
        .unwrap_or(PreviewFormat::None)
    {
        PreviewFormat::None => (),
        PreviewFormat::Jpeg => {
            attach_jpeg(&mut alert, &event);
        }
        PreviewFormat::Gif => {
            attach_gif(&mut alert, &event, frame_rate);
        }
        PreviewFormat::Webp => {
            attach_webp(&mut alert, &event, frame_rate).await;
            if alert.attachment_type.is_none() {
                info!("falling back from webp to gif due to encoding issue");
                attach_gif(&mut alert, &event, frame_rate);
            }
        }
    }

    tokio::spawn(async move {
        alert.push().await;
    });
}
