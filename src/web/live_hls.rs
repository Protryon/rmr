use std::{collections::HashMap, process::Stdio, sync::Arc, time::Duration};

use anyhow::{anyhow, Result};
use axum::{
    body::{Body, BoxBody, Bytes, Full, HttpBody},
    extract::Path,
    response::Response,
};
use axum_util::errors::{ApiError, ApiResult};
use log::{error, info};
use serde::Deserialize;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    sync::{Notify, RwLock},
};
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::config::{CameraConfig, CameraMode, CONFIG};

lazy_static::lazy_static! {
    static ref HLS: RwLock<HashMap<Uuid, Arc<Notify>>> = RwLock::new(HashMap::default());
}

async fn start_hls_manager(camera: &'static CameraConfig) -> ApiResult<Uuid> {
    let uuid = Uuid::new_v4();
    let notify = Arc::new(Notify::new());

    HLS.write().await.insert(uuid, notify.clone());

    let path = CONFIG.live_dir.join(uuid.to_string()).join("playlist.m3u8");

    tokio::spawn(async move {
        if let Err(e) = hls_manager(uuid, camera, notify).await {
            error!("[{uuid}] HLS failed: {e:#}");
        }
    });

    tokio::time::timeout(Duration::from_secs(15), async move {
        loop {
            match tokio::fs::try_exists(&path).await {
                Err(e) => {
                    error!("failed to check playlist existence: {e}");
                    break Err(ApiError::Other(e.into()));
                }
                Ok(false) => continue,
                Ok(true) => break Ok(uuid),
            }
        }
    })
    .await
    .unwrap_or_else(|_| Err(ApiError::Other(anyhow!("timeout on loading stream"))))
}

async fn hls_manager(uuid: Uuid, camera: &CameraConfig, notify: Arc<Notify>) -> Result<()> {
    let path = CONFIG.live_dir.join(uuid.to_string());
    tokio::fs::create_dir_all(&path).await?;
    let playlist = path.join("playlist.m3u8");
    let mut args = vec![];
    if CONFIG.force_tcp {
        args.extend(["-rtsp_transport", "tcp"]);
    }
    let rtsp = camera.rtsp.to_string();
    let frame_rate = (camera.frame_rate as usize).to_string();
    args.extend([
        "-i",
        &rtsp,
        "-flags",
        "+cgop",
        "-g",
        &frame_rate,
        "-c:v",
        "copy",
        "-hls_time",
        "1",
        playlist.to_str().unwrap(),
    ]);
    let mut process = Command::new(&CONFIG.ffmpeg_bin)
        .args(args)
        .stderr(Stdio::piped())
        .spawn()?;
    let stderr = process.stderr.take().unwrap();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            eprintln!("[{uuid}] {line}");
        }
    });

    defer_lite::defer! {
        let path = path.clone();
        tokio::spawn(async move {
            HLS.write().await.remove(&uuid);
            if let Err(e) = tokio::fs::remove_dir_all(&path).await {
                error!("failed to delete HLS dir '{}': {e}", path.display());
            }
        });
    }

    loop {
        match tokio::time::timeout(Duration::from_secs(10), notify.notified()).await {
            Ok(_) => (),
            Err(_) => {
                info!("[{uuid}] timeout on HLS stream, terminating");
                process.kill().await?;
                break;
            }
        }
    }

    Ok(())
}

pub async fn page(Path(name): Path<String>) -> ApiResult<Response> {
    let Some(camera) = CONFIG.cameras.get(&name) else {
        return Err(ApiError::NotFound);
    };
    if camera.mode == CameraMode::Disable {
        return Err(ApiError::NotFound);
    }

    let uuid = start_hls_manager(camera).await?;

    let total = format!(
        r#"
        <html>
        <head>
            <title>{name} Live</title>
            <script src="https://cdn.jsdelivr.net/npm/hls.js@latest"></script>
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
            <video id="video" autoplay controls muted></video>
            <script>
                if (Hls.isSupported()) {{
                    var video = document.getElementById('video');
                    var hls = new Hls();
                    // bind them together
                    hls.attachMedia(video);
                    hls.on(Hls.Events.MEDIA_ATTACHED, function () {{
                        console.log("video and hls.js are now bound together !");
                        hls.loadSource("./live_hls/{uuid}/playlist.m3u8");
                        hls.on(Hls.Events.MANIFEST_PARSED, function (event, data) {{
                        }});
                    }});
                }}
            </script>
      
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

#[derive(Deserialize)]
pub struct StreamPath {
    pub name: String,
    pub uuid: Uuid,
    pub path: String,
}

pub async fn stream(
    Path(StreamPath { name, uuid, path }): Path<StreamPath>,
) -> ApiResult<Response> {
    let Some(camera) = CONFIG.cameras.get(&name) else {
        return Err(ApiError::NotFound);
    };
    if camera.mode == CameraMode::Disable {
        return Err(ApiError::NotFound);
    }

    {
        let hls = HLS.read().await;
        let Some(notify) = hls.get(&uuid) else {
            return Err(ApiError::NotFound);
        };
        notify.notify_one();
    }
    if path.contains("/") || path.contains("..") {
        return Err(ApiError::BadRequest("malformed path".to_string()));
    }
    let filepath = CONFIG.live_dir.join(uuid.to_string()).join(&path);

    if !tokio::fs::try_exists(&filepath).await? {
        return Err(ApiError::NotFound);
    }

    let stream = tokio::fs::File::open(&filepath).await?;

    let stream = ReaderStream::new(stream);

    if path != "playlist.m3u8" {
        tokio::spawn(async move {
            if let Err(e) = tokio::fs::remove_file(&filepath).await {
                error!(
                    "failed to unlink stream fragment '{}': {e}",
                    filepath.display()
                );
            }
        });
    }

    //todo: content types?
    Ok(
        Response::builder().body(BoxBody::new(Body::wrap_stream(stream).map_err(|e| {
            error!("video stream error: {e:?}");
            axum::Error::new(e)
        })))?,
    )
}
