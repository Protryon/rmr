use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct EventMetadata {
    pub camera: String,
    pub when: DateTime<Utc>,
    pub total_score: f64,
    pub start_stream_frame_number: u64,
    pub end_stream_frame_number: u64,
}
