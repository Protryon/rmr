use axum::{
    body::{BoxBody, Bytes, Full, HttpBody},
    extract::Path,
    response::Response,
};
use axum_util::errors::{ApiError, ApiResult};
use chrono::{DateTime, Utc};
use typed_html::elements::FlowContent;
use typed_html::{dom::DOMTree, html, text};

use crate::config::{CameraMode, CONFIG};

#[allow(unused_braces)]
pub async fn list_recording(Path(name): Path<String>) -> ApiResult<Response> {
    let Some(camera) = CONFIG.cameras.get(&name) else {
        return Err(ApiError::NotFound);
    };
    if camera.mode == CameraMode::Disable {
        return Err(ApiError::NotFound);
    }

    let mut recording_dir = CONFIG.recording_dir.clone();
    recording_dir.push(&name);

    let mut out = Vec::<Box<dyn FlowContent<String>>>::new();

    out.push(html! {
        <div>
            {text!("{}", name)}
        </div>
    });
    out.push(html! {
        <div>
            <a href={&CONFIG.web_base}>{ text!("Home") }</a>
        </div>
    });
    out.push(html! {
        <div>
            <a href={format!("{}camera/{name}/live_hls", CONFIG.web_base)}>{ text!("Live (HLS)") }</a>
            <a href={format!("{}camera/{name}/live_mp4", CONFIG.web_base)} style="margin-left: 30px">{ text!("Live (MP4)") }</a>
        </div>
    });
    let mut entries = vec![];
    if tokio::fs::try_exists(&recording_dir).await? {
        let mut read_dir = tokio::fs::read_dir(&recording_dir).await?;
        while let Some(entry) = read_dir.next_entry().await? {
            let filename = entry.file_name().to_string_lossy().into_owned();
            if !filename.ends_with(".mp4") {
                continue;
            }
            let modified: DateTime<Utc> = entry.metadata().await?.modified()?.into();
            entries.push((modified, filename));
        }
    }
    entries.sort_by_key(|x| x.0);
    for (modified, filename) in entries {
        out.push(html! {
            <div>
                <a href={format!("{}camera/{name}/video/{filename}", CONFIG.web_base)}>{ text!("{} -> {}", modified, filename) }</a>
            </div>
        });
    }
    let total: DOMTree<String> = html! {
        <html>
        <head>
            <title>"RMR"</title>
            <style>
                r"
                * {
                    font-size: 72px
                }"
            </style>
        </head>
        <body>
            {out.into_iter()}
        </body>
        </html>
    };

    Ok(Response::builder()
        .header("content-type", "text/html")
        .body(BoxBody::new::<_>(
            Full::new(Bytes::from(total.to_string())).map_err(|_| unreachable!()),
        ))?)
}
