use axum::{
    body::{BoxBody, Bytes, Full, HttpBody},
    response::Response,
};
use axum_util::errors::ApiResult;
use typed_html::elements::FlowContent;
use typed_html::{dom::DOMTree, html, text};

use crate::{config::CONFIG, event::EventMetadata};

#[allow(unused_braces)]
pub async fn list_events() -> ApiResult<Response> {
    let mut out = Vec::<Box<dyn FlowContent<String>>>::new();

    out.push(html! {
        <div>
            "Events"
        </div>
    });
    out.push(html! {
        <div>
            <a href={&CONFIG.web_base}>{ text!("Home") }</a>
        </div>
    });
    let mut read_dir = tokio::fs::read_dir(&CONFIG.event_dir).await?;
    let mut entries = vec![];
    while let Some(entry) = read_dir.next_entry().await? {
        let filename = entry.file_name().to_string_lossy().into_owned();
        if !filename.ends_with(".mp4") {
            continue;
        }
        let metadata_file = CONFIG.event_dir.join(&filename).with_extension("json");
        let parsed: EventMetadata =
            serde_json::from_str(&tokio::fs::read_to_string(&metadata_file).await?)?;
        entries.push((parsed, filename));
    }
    entries.sort_by_key(|x| x.0.when);
    for (metadata, filename) in entries {
        out.push(html! {
            <div>
                <a href={format!("{}event/{filename}", CONFIG.web_base)}>{ text!("{}", filename) }</a>
                {text!(": {} score, {} frames in {}", metadata.total_score, metadata.end_stream_frame_number.saturating_sub(metadata.start_stream_frame_number), metadata.camera) }
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
