use std::sync::Arc;

use axum::{routing, Router};
use axum_util::logger::{LoggerConfig, LoggerLayer};
use log::Level;

mod get_event;
mod get_video;
mod list_camera;
mod list_events;
mod list_recording;
mod live;

async fn health() {}

pub fn route() -> Router {
    Router::new()
        .route("/", routing::get(list_camera::list_camera))
        .route(
            "/camera/:name",
            routing::get(list_recording::list_recording),
        )
        .route("/events", routing::get(list_events::list_events))
        .route("/events/:filename", routing::get(get_event::get_event))
        .route(
            "/camera/:name/video/:filename",
            routing::get(get_video::get_video),
        )
        .route("/camera/:name/live", routing::get(live::page))
        .route("/camera/:name/live/:uuid/:path", routing::get(live::stream))
        .route("/health", routing::get(health))
        .layer(LoggerLayer::new(LoggerConfig {
            log_level_filter: Arc::new(|x| {
                if x == "/health" {
                    Level::Debug
                } else {
                    Level::Info
                }
            }),
            honor_xff: true,
            metric_name: "rmr_web_responses".to_string(),
        }))
}
