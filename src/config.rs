use std::{net::SocketAddr, path::PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::modect::RunningMotionDetectorConfig;

fn default_ffmpeg_bin() -> String {
    "ffmpeg".to_string()
}

fn default_web_base() -> String {
    "/".to_string()
}

#[derive(Serialize, Deserialize)]
pub struct Config {
    pub prometheus_bind: Option<SocketAddr>,
    pub web_bind: SocketAddr,
    #[serde(default = "default_web_base")]
    pub web_base: String,
    pub cameras: IndexMap<String, CameraConfig>,
    #[serde(default = "default_ffmpeg_bin")]
    pub ffmpeg_bin: String,
    pub recording_dir: PathBuf,
    pub event_dir: PathBuf,
    pub live_dir: PathBuf,
    // if true, ffmpeg is forced to use TCP (useful on k8s)
    #[serde(default)]
    pub force_tcp: bool,
    pub pushover: Option<PushoverConfig>,
}

fn default_frame_rate() -> f64 {
    25.0
}

#[derive(Serialize, Deserialize)]
pub struct CameraConfig {
    pub rtsp: Url,
    pub mode: CameraMode,
    #[serde(default = "default_frame_rate")]
    pub frame_rate: f64,
    pub motion_detection: Option<MotionDetectionConfig>,
}

fn default_pushover() -> Url {
    "https://api.pushover.net/1/messages.json".parse().unwrap()
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default)]
#[serde(rename_all = "snake_case")]
#[repr(i32)]
pub enum PushoverPriority {
    Ignore = -3,
    Lowest = -2,
    Low = -1,
    #[default]
    Normal = 0,
    High = 1,
    Emergency = 2,
}

#[derive(Serialize, Deserialize)]
pub struct PushoverConfig {
    #[serde(default = "default_pushover")]
    pub url: Url,
    pub user_key: String,
    pub token: String,
    #[serde(default)]
    pub preview_format: PreviewFormat,
    #[serde(default)]
    pub priority: PushoverPriority,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MotionDetectionConfig {
    pub width: u32,
    pub height: u32,
    #[serde(flatten)]
    pub config: RunningMotionDetectorConfig,
    #[serde(default)]
    pub alert_priority: Option<PushoverPriority>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PreviewFormat {
    None,
    Jpeg,
    Gif,
    #[default]
    Webp,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CameraMode {
    Disable,
    Record,
    MotionDetect,
    MotionDetectRecord,
}

lazy_static::lazy_static! {
    static ref CONFIG_PATH: PathBuf = {
        let var = std::env::var("RMR_CONFIG").unwrap_or_default();
        if var.is_empty() {
            "./config.yaml".parse().unwrap()
        } else {
            var.parse().expect("invalid config path")
        }
    };
    pub static ref CONFIG: Config = {
        serde_yaml::from_str(&std::fs::read_to_string(&*CONFIG_PATH).expect("failed to read config file")).expect("failed to parse config file")
    };
}
