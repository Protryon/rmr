use axum::{extract::Path, headers::Range, response::Response, TypedHeader};
use axum_util::errors::{ApiError, ApiResult};

use crate::config::CONFIG;

use super::get_video::stream_video;

pub async fn get_event(
    Path(filename): Path<String>,
    range: Option<TypedHeader<Range>>,
) -> ApiResult<Response> {
    if !filename.ends_with(".mp4") {
        return Err(ApiError::NotFound);
    }
    let mut video_path = CONFIG.event_dir.clone();
    if filename.contains("/") || filename.contains("..") {
        return Err(ApiError::NotFound);
    }
    video_path.push(filename);

    stream_video(&video_path, range).await
}
