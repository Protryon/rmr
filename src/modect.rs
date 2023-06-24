use chrono::{DateTime, Utc};
use image::{GrayImage, RgbImage};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RunningMotionDetectorConfig {
    pub change_minimum: f64,
    pub change_maximum: f64,
    pub stddev_minimum: f64,
    pub minimum_frame_count: usize,
    pub minimum_total_change: f64,
    pub followup_frame_count: usize,
    pub maximum_frame_wait: usize,
    pub mask_file: Option<String>,
}

pub struct RunningMotionDetector {
    mask_image: Option<GrayImage>,
    config: RunningMotionDetectorConfig,
    last_frame: Option<RgbImage>,
    frame_number: u64,
    motion_detector: MotionDetector,
    current_detection: Vec<MotionDetectionFrame>,
    followup_frames: Vec<MotionDetectionFrame>,
    detection_start_frame: Option<u64>,
    detection_confirmed: bool,
    current_detection_score: f64,
    pending_states: Vec<(DateTime<Utc>, MotionDetectionState)>,
}

#[derive(Clone)]
pub struct MotionDetectionFrame {
    pub image: RgbImage,
    pub change: f64,
    pub stddev: f64,
}

pub struct MotionDetectionEvent {
    pub start_stream_frame_number: u64,
    pub end_stream_frame_number: u64,
    pub frames: Vec<MotionDetectionFrame>,
    pub total_score: f64,
}

#[repr(u16)]
pub enum MotionDetectionState {
    Idle {
        frame_number: u64,
    },
    WaitAndSee {
        start_frame_number: u64,
        current_frame_number: u64,
        current_score: f64,
    },
    Active {
        start_frame_number: u64,
        current_frame_number: u64,
        current_score: f64,
    },
    Followup {
        start_frame_number: u64,
        current_frame_number: u64,
        current_score: f64,
    },
    Rejected {
        event: MotionDetectionEvent,
    },
    Completed {
        event: MotionDetectionEvent,
        was_confirmed_already: bool,
    },
    ConfirmedInProgress {
        event: MotionDetectionEvent,
    },
}

impl MotionDetectionState {
    pub fn discriminant(&self) -> u16 {
        unsafe { *(self as *const Self as *const u16) }
    }
}

pub struct MotionDetectionStats {
    pub change: f64,
    pub stddev: f64,
    pub frame_number: u64,
}

impl RunningMotionDetector {
    pub fn new(config: RunningMotionDetectorConfig) -> Self {
        Self {
            mask_image: config
                .mask_file
                .as_ref()
                .map(|x| image::open(x).expect("failed to open mask").to_luma8()),
            config,
            last_frame: None,
            frame_number: 0,
            motion_detector: MotionDetector {},
            current_detection: vec![],
            followup_frames: vec![],
            current_detection_score: 0.0,
            pending_states: vec![],
            detection_start_frame: None,
            detection_confirmed: false,
        }
    }

    pub fn drain_pending_states<'a>(
        &'a mut self,
    ) -> impl Iterator<Item = (DateTime<Utc>, MotionDetectionState)> + 'a {
        self.pending_states.drain(..)
    }

    pub fn frame_recv(&mut self, new_frame: RgbImage) -> MotionDetectionStats {
        let Some(last_frame) = self.last_frame.as_ref() else {
            self.pending_states.push((Utc::now(), MotionDetectionState::Idle { frame_number: self.frame_number }));
            self.last_frame = Some(new_frame);
            self.frame_number += 1;
            return MotionDetectionStats {
                change: 0.0,
                stddev: 0.0,
                frame_number: self.frame_number - 1,
            };
        };
        let diff =
            self.motion_detector
                .frame_diff(last_frame, &new_frame, self.mask_image.as_ref());
        if diff.average > self.config.change_minimum
            && diff.average < self.config.change_maximum
            && diff.std_dev_estimate > self.config.stddev_minimum
        {
            if !self.followup_frames.is_empty() {
                self.current_detection
                    .extend(self.followup_frames.drain(..));
            } else if self.current_detection.is_empty() {
                self.current_detection.push(MotionDetectionFrame {
                    image: last_frame.clone(),
                    change: 0.0,
                    stddev: 0.0,
                });
            }
            if self.detection_start_frame.is_none() {
                self.detection_start_frame = Some(self.frame_number - 1);
            }
            if self.current_detection_score >= self.config.minimum_total_change
                && self.frame_number - self.detection_start_frame.unwrap()
                    > (self.config.maximum_frame_wait
                        + self.config.followup_frame_count
                        + self.config.minimum_frame_count) as u64
                && !self.detection_confirmed
            {
                self.detection_confirmed = true;
                self.pending_states.push((
                    Utc::now(),
                    MotionDetectionState::ConfirmedInProgress {
                        event: MotionDetectionEvent {
                            start_stream_frame_number: self.detection_start_frame.unwrap(),
                            end_stream_frame_number: self.frame_number,
                            frames: self.current_detection.clone(),
                            total_score: self.current_detection_score,
                        },
                    },
                ));
            }
            self.current_detection.push(MotionDetectionFrame {
                image: new_frame.clone(),
                change: diff.average,
                stddev: diff.std_dev_estimate,
            });
            self.current_detection_score += diff.average;

            if self.current_detection.len() <= self.config.minimum_frame_count
                || self.current_detection_score < self.config.minimum_total_change
            {
                self.pending_states.push((
                    Utc::now(),
                    MotionDetectionState::WaitAndSee {
                        start_frame_number: self.detection_start_frame.unwrap(),
                        current_frame_number: self.frame_number,
                        current_score: self.current_detection_score,
                    },
                ));
            } else {
                self.pending_states.push((
                    Utc::now(),
                    MotionDetectionState::Active {
                        start_frame_number: self.detection_start_frame.unwrap(),
                        current_frame_number: self.frame_number,
                        current_score: self.current_detection_score,
                    },
                ));
            }
        } else if !self.current_detection.is_empty() {
            if self.followup_frames.len() < self.config.followup_frame_count {
                self.pending_states.push((
                    Utc::now(),
                    MotionDetectionState::Followup {
                        start_frame_number: self.detection_start_frame.unwrap(),
                        current_frame_number: self.frame_number,
                        current_score: self.current_detection_score,
                    },
                ));
                self.followup_frames.push(MotionDetectionFrame {
                    image: new_frame.clone(),
                    change: 0.0,
                    stddev: 0.0,
                });
            } else if self.current_detection.len() <= self.config.minimum_frame_count
                || self.current_detection_score < self.config.minimum_total_change
            {
                self.current_detection
                    .extend(self.followup_frames.drain(..));
                self.pending_states.push((
                    Utc::now(),
                    MotionDetectionState::Rejected {
                        event: MotionDetectionEvent {
                            start_stream_frame_number: self.detection_start_frame.take().unwrap(),
                            end_stream_frame_number: self.frame_number - 1,
                            frames: self.current_detection.drain(..).collect(),
                            total_score: self.current_detection_score,
                        },
                    },
                ));
                self.current_detection_score = 0.0;
                self.detection_confirmed = false;
                self.pending_states.push((
                    Utc::now(),
                    MotionDetectionState::Idle {
                        frame_number: self.frame_number,
                    },
                ));
            } else {
                self.current_detection
                    .extend(self.followup_frames.drain(..));
                self.pending_states.push((
                    Utc::now(),
                    MotionDetectionState::Completed {
                        was_confirmed_already: self.detection_confirmed,
                        event: MotionDetectionEvent {
                            start_stream_frame_number: self.detection_start_frame.take().unwrap(),
                            end_stream_frame_number: self.frame_number - 1,
                            frames: self.current_detection.drain(..).collect(),
                            total_score: self.current_detection_score,
                        },
                    },
                ));
                self.current_detection_score = 0.0;
                self.detection_confirmed = false;
                self.pending_states.push((
                    Utc::now(),
                    MotionDetectionState::Idle {
                        frame_number: self.frame_number,
                    },
                ));
            }
        }
        self.frame_number += 1;
        self.last_frame = Some(new_frame);
        MotionDetectionStats {
            change: diff.average,
            stddev: diff.std_dev_estimate,
            frame_number: self.frame_number - 1,
        }
    }
}

pub struct MotionDetector {}

pub struct MotionDetectionResult {
    pub average: f64,
    pub std_dev_estimate: f64,
}

impl MotionDetector {
    pub fn frame_diff(
        &self,
        frame1: &RgbImage,
        frame2: &RgbImage,
        mask: Option<&GrayImage>,
    ) -> MotionDetectionResult {
        assert_eq!(frame1.len(), frame2.len());
        let mut sum = 0u64;
        let mut running_stddev = 0.0f64;
        let mut pixel_ct = 0u64;
        let mut mask_iter = mask.map(|x| x.pixels());
        for (pixel1, pixel2) in frame1.pixels().zip(frame2.pixels()) {
            let mask = mask_iter
                .as_mut()
                .and_then(|x| x.next())
                .map(|x| x.0[0] == 0)
                .unwrap_or(true);
            if !mask {
                continue;
            }
            let diff = pixel1
                .0
                .iter()
                .zip(pixel2.0.iter())
                .map(|(c1, c2)| ((*c1 as i32) - (*c2 as i32)).pow(2))
                .sum::<i32>();
            if pixel_ct > 0 {
                running_stddev += (diff as f64 - (sum as f64 / pixel_ct as f64)).powi(2);
            }
            pixel_ct += 1;
            sum += diff as u64;
        }
        MotionDetectionResult {
            average: sum as f64 / pixel_ct as f64,
            std_dev_estimate: (running_stddev / pixel_ct as f64).sqrt(),
        }
    }

    #[allow(dead_code)]
    pub fn frame_diff_img(&self, frame1: &RgbImage, frame2: &RgbImage) -> RgbImage {
        let mut out = frame1.clone();
        for ((pixel1, pixel2), out) in frame1.pixels().zip(frame2.pixels()).zip(out.pixels_mut()) {
            let diff = pixel1
                .0
                .iter()
                .zip(pixel2.0.iter())
                .map(|(c1, c2)| ((*c1 as i32) - (*c2 as i32)).pow(2))
                .sum::<i32>() as u64;
            let x = (diff as f64).sqrt().min(255.0) as u8;
            out.0 = [x, x, x];
        }
        out
    }
}
