use std::{
    pin::Pin,
    process::Stdio,
    task::{Context, Poll},
};

use axum::{
    body::{Body, BoxBody, Bytes, Full, HttpBody},
    extract::Path,
    response::Response,
};
use axum_util::errors::{ApiError, ApiResult};
use futures::Stream;
use log::error;
use pin_project::{pin_project, pinned_drop};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, ChildStdout, Command},
};
use tokio_util::io::ReaderStream;

use crate::config::{CameraConfig, CameraMode, CONFIG};

async fn run_mp4(camera: &CameraConfig) -> ApiResult<(Child, ChildStdout)> {
    let mut args = vec![];
    if CONFIG.force_tcp {
        args.extend(["-rtsp_transport", "tcp"]);
    }
    let rtsp = camera.rtsp.to_string();
    args.extend([
        "-i",
        &rtsp,
        "-flags",
        "+cgop",
        "-f",
        "mp4",
        "-movflags",
        "frag_keyframe+empty_moov",
        "-c:v",
        "copy",
        "-",
    ]);
    let mut process = Command::new(&CONFIG.ffmpeg_bin)
        .args(args)
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let stderr = process.stderr.take().unwrap();
    let stdout = process.stdout.take().unwrap();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            eprintln!("ffmpeg for mp4 stream: {line}");
        }
    });

    Ok((process, stdout))
}

pub async fn page(Path(name): Path<String>) -> ApiResult<Response> {
    let Some(camera) = CONFIG.cameras.get(&name) else {
        return Err(ApiError::NotFound);
    };
    if camera.mode == CameraMode::Disable {
        return Err(ApiError::NotFound);
    }

    let total = format!(
        r#"
        <html>
        <head>
            <title>{name} Live</title>
            <style>
            #video {{
                object-fit: contain;
            }}
            * {{
                font-size: 36px
            }}
            </style>
        </head>
        <body>
            <div>
                {name} <a href="{0}">Home</a> <a href="{0}camera/{name}">Recordings</a>
            </div>
            <video id="video" autoplay controls muted>
                <source src="./live_mp4/stream.mp4" type="video/mp4">
            </video>
        </body>
        </html>
    "#,
        CONFIG.web_base
    );

    Ok(Response::builder()
        .header("content-type", "text/html")
        .body(BoxBody::new::<_>(
            Full::new(Bytes::from(total)).map_err(|_| unreachable!()),
        ))?)
}

#[pin_project(PinnedDrop)]
struct FfmpegStream {
    #[pin]
    stdout: ReaderStream<ChildStdout>,
    process: Child,
}

impl Stream for FfmpegStream {
    type Item = std::io::Result<Bytes>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        this.stdout.poll_next(cx)
    }
}

#[pinned_drop]
impl PinnedDrop for FfmpegStream {
    fn drop(mut self: Pin<&mut Self>) {
        if let Err(e) = self.process.start_kill() {
            error!("failed to kill ffmpeg: {e}");
        }
    }
}

pub async fn stream(Path(name): Path<String>) -> ApiResult<Response> {
    let Some(camera) = CONFIG.cameras.get(&name) else {
        return Err(ApiError::NotFound);
    };
    if camera.mode == CameraMode::Disable {
        return Err(ApiError::NotFound);
    }

    let (process, stdout) = run_mp4(camera).await?;

    let stream = FfmpegStream {
        stdout: ReaderStream::new(stdout),
        process,
    };

    //todo: content types?
    Ok(
        Response::builder().body(BoxBody::new(Body::wrap_stream(stream).map_err(|e| {
            error!("video stream error: {e:?}");
            axum::Error::new(e)
        })))?,
    )
}
