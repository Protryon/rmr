use clap::Parser;
use config::{CameraMode, CONFIG};
use image::RgbImage;
use log::{debug, error, info, trace};
use modect::{MotionDetectionState, RunningMotionDetector};
use prometheus::{
    register_counter_vec, register_histogram_vec, register_int_counter_vec, register_int_gauge_vec,
    CounterVec, HistogramVec, IntCounterVec, IntGaugeVec,
};
use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

use crate::pushover::{alert_event, AlertState};

mod config;
mod event;
mod ffmpeg;
mod modect;
mod modect_mp4;
mod observable_buf;
mod pushover;
mod web;

lazy_static::lazy_static! {
    static ref FRAME_COUNTER: IntGaugeVec = register_int_gauge_vec!("rmr_frame_counter", "stream frame counter", &["camera"]).unwrap();
    static ref MODECT_CHANGE: CounterVec = register_counter_vec!("rmr_modect_change", "frame change value", &["camera"]).unwrap();
    static ref MODECT_STDDEV: CounterVec = register_counter_vec!("rmr_modect_stddev", "frame change std dev (estd)", &["camera"]).unwrap();
    static ref MODECT_REJECT: CounterVec = register_counter_vec!("rmr_modect_reject", "count of events rejected by filter", &["camera"]).unwrap();
    static ref MODECT_REJECT_SCORE: HistogramVec = register_histogram_vec!("rmr_modect_reject_score", "rejection total scores", &["camera"]).unwrap();
    static ref MODECT_CONFIRM: CounterVec = register_counter_vec!("rmr_modect_confirm", "confirmation of events accepted by filter (before completion)", &["camera"]).unwrap();
    static ref MODECT_COMPLETE: CounterVec = register_counter_vec!("rmr_modect_complete", "count of events accepted by filter", &["camera"]).unwrap();
    static ref MODECT_COMPLETE_SCORE: HistogramVec = register_histogram_vec!("rmr_modect_complete_score", "accepted total scores", &["camera"]).unwrap();
    static ref MODECT_LAST_REJECT: IntGaugeVec = register_int_gauge_vec!("rmr_modect_last_reject", "last rejection frame", &["camera"]).unwrap();
    static ref MODECT_LAST_COMPLETE: IntGaugeVec = register_int_gauge_vec!("rmr_modect_last_complete", "last accepted frame", &["camera"]).unwrap();
    static ref MODECT_STATE: IntGaugeVec = register_int_gauge_vec!("rmr_modect_state", "current state", &["camera"]).unwrap();
    static ref MODECT_ALERT_LATENCY: CounterVec = register_counter_vec!("rmr_modect_alert_latency_ms", "ms latency of sending alerts, including encoding", &["camera"]).unwrap();
    static ref MODECT_ALERT_COUNT: IntCounterVec = register_int_counter_vec!("rmr_modect_alert_count", "count of alerts sent", &["camera"]).unwrap();

    static ref ARGS: Args = Args::parse();
}

/// Rust Monitor & Record
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Dumps a screenshot into the recording directory for each non-disabled camera
    #[clap(short, long)]
    snapshot: bool,
}

#[tokio::main]
async fn main() {
    lazy_static::initialize(&ARGS);

    env_logger::Builder::new()
        .parse_env(env_logger::Env::default().default_filter_or("info"))
        .init();

    if ARGS.snapshot {
        for (name, camera) in &CONFIG.cameras {
            if matches!(camera.mode, CameraMode::Disable) {
                continue;
            }
            let mut recording_dir = CONFIG.recording_dir.clone();
            recording_dir.push(name);
            tokio::fs::create_dir_all(&recording_dir).await.unwrap();

            ffmpeg::FFmpegConfig {
                binary: CONFIG.ffmpeg_bin.clone(),
                rtsp_input: camera.rtsp.clone(),
                recording_mp4_dir: Some(recording_dir),
                send_images: None,
                image_width: camera.motion_detection.as_ref().map(|x| x.width),
                image_height: camera.motion_detection.as_ref().map(|x| x.height),
                record_single_jpeg: true,
            }
            .run()
            .await
            .unwrap();
        }
        return;
    }

    if let Some(prometheus_bind) = CONFIG.prometheus_bind {
        prometheus_exporter::start(prometheus_bind).expect("failed to load prometheus_exporter");
    }

    tokio::spawn(async move {
        async fn run() -> anyhow::Result<()> {
            let server = axum::Server::bind(&CONFIG.web_bind);
            info!("listening @ {}", CONFIG.web_bind);
            server
                .serve(web::route().into_make_service_with_connect_info::<SocketAddr>())
                .await?;
            Ok(())
        }
        loop {
            if let Err(e) = run().await {
                error!("failed to start api server: {:?}", e);
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });

    let mut tasks = vec![];

    for (name, camera) in &CONFIG.cameras {
        tasks.push(tokio::spawn(async move {

            match camera.mode {
                CameraMode::Disable => return,
                CameraMode::Record => {
                    let mut recording_dir = CONFIG.recording_dir.clone();
                    recording_dir.push(name);
                    tokio::fs::create_dir_all(&recording_dir).await.unwrap();

                    ffmpeg::FFmpegConfig {
                        binary: CONFIG.ffmpeg_bin.clone(),
                        rtsp_input: camera.rtsp.clone(),
                        recording_mp4_dir: Some(recording_dir),
                        send_images: None,
                        image_width: camera.motion_detection.as_ref().map(|x| x.width),
                        image_height: camera.motion_detection.as_ref().map(|x| x.height),
                        record_single_jpeg: false,
                    }.run().await.unwrap();
                },
                CameraMode::MotionDetect => {
                    let Some(motion_detection_config) = &camera.motion_detection else {
                        panic!("missing motion detection configuration for motion detection camera");
                    };
                    let motion_detect_dir = CONFIG.event_dir.clone();
                    tokio::fs::create_dir_all(&motion_detect_dir).await.unwrap();

                    let (sender, mut receiver) = mpsc::channel::<RgbImage>(10);
                    let mut motion_detector = RunningMotionDetector::new(motion_detection_config.config.clone());

                    let camera_alert_priority = motion_detection_config.alert_priority;
                    let frame_rate = camera.frame_rate;

                    let camera_name = name.clone();
                    tokio::spawn(async move {
                        while let Some(new_frame) = receiver.recv().await {
                            let stats = motion_detector.frame_recv(new_frame);
                            debug!("{camera_name}: f#{} score={:.02}, stddev = {:.02}", stats.frame_number, stats.change, stats.stddev);
                            FRAME_COUNTER.with_label_values(&[&camera_name]).set(stats.frame_number as i64);
                            MODECT_CHANGE.with_label_values(&[&camera_name]).inc_by(stats.change);
                            MODECT_STDDEV.with_label_values(&[&camera_name]).inc_by(stats.stddev);
                            for (time, state) in motion_detector.drain_pending_states() {
                                MODECT_STATE.with_label_values(&[&camera_name]).set(state.discriminant() as i64);
                                match state {
                                    MotionDetectionState::Idle { frame_number } => {
                                        trace!("{camera_name}: f#{frame_number} idle");
                                    },
                                    MotionDetectionState::Rejected { event } => {
                                        MODECT_REJECT.with_label_values(&[&camera_name]).inc();
                                        MODECT_REJECT_SCORE.with_label_values(&[&camera_name]).observe(event.total_score);
                                        MODECT_LAST_REJECT.with_label_values(&[&camera_name]).set(event.end_stream_frame_number as i64);
                                        info!("{camera_name}: f#{} -> f#{} rejected ({} frames, {:.02} score)", event.start_stream_frame_number, event.end_stream_frame_number, event.frames.len(), event.total_score);
                                    },
                                    MotionDetectionState::WaitAndSee { start_frame_number, current_frame_number, current_score } => {
                                        debug!("{camera_name}: f#{start_frame_number} -> f#{current_frame_number} wait_and_see ({:.02} score)", current_score);
                                    },
                                    MotionDetectionState::Active { start_frame_number, current_frame_number, current_score } => {
                                        info!("{camera_name}: f#{start_frame_number} -> f#{current_frame_number} active ({:.02} score)", current_score);
                                    },
                                    MotionDetectionState::Followup { start_frame_number, current_frame_number, current_score } => {
                                        debug!("{camera_name}: f#{start_frame_number} -> f#{current_frame_number} followup ({:.02} score)", current_score);
                                    },
                                    MotionDetectionState::ConfirmedInProgress { event } => {
                                        MODECT_CONFIRM.with_label_values(&[&camera_name]).inc();

                                        info!("{camera_name}: f#{} -> f#{} confirmed ({} frames, {:.02} score)", event.start_stream_frame_number, event.end_stream_frame_number, event.frames.len(), event.total_score);

                                        let event = Arc::new(event);
                                        let camera_name = camera_name.clone();
                                        tokio::spawn(async move {
                                            let start = Instant::now();
                                            alert_event(time, event, camera_alert_priority, &camera_name, frame_rate, AlertState::Confirmed).await;
                                            let ms = start.elapsed().as_secs_f64() * 1000.0;
                                            MODECT_ALERT_LATENCY.with_label_values(&[&camera_name]).inc_by(ms);
                                            MODECT_ALERT_COUNT.with_label_values(&[&camera_name]).inc();
                                            info!("Alert sent in {ms:.02} ms");
                                        });
                                    },
                                    MotionDetectionState::Completed { was_confirmed_already, event } => {
                                        MODECT_COMPLETE.with_label_values(&[&camera_name]).inc();
                                        MODECT_COMPLETE_SCORE.with_label_values(&[&camera_name]).observe(event.total_score);
                                        MODECT_LAST_COMPLETE.with_label_values(&[&camera_name]).set(event.end_stream_frame_number as i64);

                                        info!("{camera_name}: f#{} -> f#{} completed ({} frames, {:.02} score)", event.start_stream_frame_number, event.end_stream_frame_number, event.frames.len(), event.total_score);
                                        let event_path = motion_detect_dir.join(&format!("{}_{}.mp4", camera_name, time));

                                        let event = Arc::new(event);
                                        let event2 = event.clone();
                                        let camera_name = camera_name.clone();
                                        tokio::spawn(async move {
                                            let start = Instant::now();
                                            alert_event(time, event2, camera_alert_priority, &camera_name, frame_rate, if was_confirmed_already {
                                                AlertState::CompletedAfterConfirm
                                            } else {
                                                AlertState::Completed
                                            }).await;
                                            let ms = start.elapsed().as_secs_f64() * 1000.0;
                                            MODECT_ALERT_LATENCY.with_label_values(&[&camera_name]).inc_by(ms);
                                            MODECT_ALERT_COUNT.with_label_values(&[&camera_name]).inc();
                                            info!("Alert sent in {ms:.02} ms");
                                        });
                                        tokio::spawn(async move {
                                            if let Err(e) = modect_mp4::modect_mp4(&event, frame_rate as u32, &event_path).await {
                                                error!("failed to save event to disk: {e:#}");
                                            }
                                        });
                                    },
                                }
                            }
                        }
                    });

                    loop {
                        let out = ffmpeg::FFmpegConfig {
                            binary: CONFIG.ffmpeg_bin.clone(),
                            rtsp_input: camera.rtsp.clone(),
                            recording_mp4_dir: None,
                            send_images: Some(sender.clone()),
                            image_width: camera.motion_detection.as_ref().map(|x| x.width),
                            image_height: camera.motion_detection.as_ref().map(|x| x.height),
                            record_single_jpeg: false,
                        }.run().await;
                        if let Err(e) = out {
                            error!("ffmpeg failed: {e}");
                        }
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                },
                CameraMode::MotionDetectRecord => todo!(),
            }
        }));
    }
    let _ = futures::future::select_all(tasks).await;
}
