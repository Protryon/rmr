use std::{io::SeekFrom, ops::Bound};

use axum::{
    body::{Body, BoxBody, HttpBody},
    extract::Path,
    headers::{ContentRange, HeaderMapExt, Range},
    response::Response,
    TypedHeader,
};
use axum_util::errors::{ApiError, ApiResult};
use log::error;
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use crate::config::{CameraMode, CONFIG};

#[derive(Deserialize)]
pub struct VideoPath {
    pub name: String,
    pub filename: String,
}

pub async fn stream_video(
    video_path: &std::path::Path,
    range: Option<TypedHeader<Range>>,
) -> ApiResult<Response> {
    if !tokio::fs::try_exists(&video_path).await? {
        return Err(ApiError::NotFound);
    }

    let mut stream = tokio::fs::File::open(&video_path).await?;

    let length = stream.seek(SeekFrom::End(0)).await?;
    stream.seek(SeekFrom::Start(0)).await?;

    let range = range.and_then(|range| range.0.iter().next());

    let mut bounded_length = length;

    let stream = if let Some((start, end)) = range {
        let start = match start {
            Bound::Included(i) => i,
            Bound::Excluded(i) => i + 1,
            Bound::Unbounded => 0,
        };
        let end = match end {
            Bound::Included(i) => i + 1,
            Bound::Excluded(i) => i,
            Bound::Unbounded => length,
        };
        if start > end || end > length {
            return Err(ApiError::BadRequest("invalid range".to_string()));
        }
        stream.seek(SeekFrom::Start(start)).await?;
        bounded_length = end - start;
        stream.take(end - start)
    } else {
        stream.take(length)
    };

    let stream = ReaderStream::new(stream);

    let mut response = Response::builder()
        .header("content-type", "video/mp4")
        .header("accept-ranges", "bytes")
        .header("content-length", bounded_length.to_string());

    if let Some((start, end)) = range {
        response = response.status(206);

        response
            .headers_mut()
            .unwrap()
            .typed_insert(ContentRange::bytes((start, end), length)?);
    }

    Ok(
        response.body(BoxBody::new(Body::wrap_stream(stream).map_err(|e| {
            error!("video stream error: {e:?}");
            axum::Error::new(e)
        })))?,
    )
}

pub async fn get_video(
    Path(VideoPath { name, filename }): Path<VideoPath>,
    range: Option<TypedHeader<Range>>,
) -> ApiResult<Response> {
    let Some(camera) = CONFIG.cameras.get(&name) else {
        return Err(ApiError::NotFound);
    };
    if camera.mode == CameraMode::Disable {
        return Err(ApiError::NotFound);
    }

    let mut video_path = CONFIG.recording_dir.clone();
    video_path.push(&name);
    if filename.contains("/") || filename.contains("..") {
        return Err(ApiError::NotFound);
    }
    video_path.push(filename);

    stream_video(&video_path, range).await
}
