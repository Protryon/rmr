use axum::{
    body::{BoxBody, Bytes, Full, HttpBody},
    response::Response,
};
use axum_util::errors::ApiResult;
use typed_html::elements::FlowContent;
use typed_html::{dom::DOMTree, html, text};

use crate::config::{CameraMode, CONFIG};

#[allow(unused_braces)]
pub async fn list_camera() -> ApiResult<Response> {
    let mut out = Vec::<Box<dyn FlowContent<String>>>::new();

    out.push(html! {
        <div>
            <a href={format!("{}events", CONFIG.web_base)}>{ text!("Events") }</a>
        </div>
    });
    for (name, camera) in &CONFIG.cameras {
        if camera.mode == CameraMode::Disable {
            continue;
        }
        out.push(html! {
            <div>
                {text!("{}: ", name)} <a href={format!("{}camera/{name}/live", CONFIG.web_base)}>{ text!("Live") }</a>
                <a href={format!("{}camera/{name}", CONFIG.web_base)} style="margin-left: 30px">{ text!("Recordings") }</a>
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
