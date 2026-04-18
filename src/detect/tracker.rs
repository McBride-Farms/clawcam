use std::time::{Duration, Instant};

use crate::webhook::Detection;

#[derive(Debug, Clone)]
pub struct TrackedObject {
    pub track_id: u64,
    pub class: String,
    pub class_id: u32,
    pub bbox: BBox,
    pub score: f32,
    pub first_seen: Instant,
    pub last_seen: Instant,
    pub frames_seen: u32,
    prev_center: (f32, f32),
    total_movement: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct BBox {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

impl BBox {
    fn center(&self) -> (f32, f32) {
        (
            (self.left + self.right) as f32 / 2.0,
            (self.top + self.bottom) as f32 / 2.0,
        )
    }

    fn area(&self) -> f32 {
        (self.right.saturating_sub(self.left) * self.bottom.saturating_sub(self.top)) as f32
    }

    fn iou(&self, other: &BBox) -> f32 {
        let x1 = self.left.max(other.left) as f32;
        let y1 = self.top.max(other.top) as f32;
        let x2 = self.right.min(other.right) as f32;
        let y2 = self.bottom.min(other.bottom) as f32;

        let inter = (x2 - x1).max(0.0) * (y2 - y1).max(0.0);
        let union = self.area() + other.area() - inter;
        if union <= 0.0 { 0.0 } else { inter / union }
    }
}

impl From<&Detection> for BBox {
    fn from(d: &Detection) -> Self {
        BBox { left: d.left, top: d.top, right: d.right, bottom: d.bottom }
    }
}

impl TrackedObject {
    pub fn duration(&self) -> Duration {
        self.last_seen.duration_since(self.first_seen)
    }

    pub fn movement(&self) -> f32 {
        self.total_movement
    }

    pub fn is_stationary(&self, threshold_px: f32) -> bool {
        // Average movement per frame
        if self.frames_seen <= 1 {
            return true;
        }
        self.total_movement / self.frames_seen as f32 <= threshold_px
    }
}

const IOU_MATCH_THRESHOLD: f32 = 0.25;
const MAX_STALE_AGE: Duration = Duration::from_secs(2);

pub struct ObjectTracker {
    tracks: Vec<TrackedObject>,
    next_id: u64,
}

impl ObjectTracker {
    pub fn new() -> Self {
        Self {
            tracks: Vec::new(),
            next_id: 1,
        }
    }

    /// Update tracker with new detections. Returns current active tracks.
    pub fn update(&mut self, detections: &[Detection]) -> Vec<TrackedObject> {
        let now = Instant::now();
        let mut det_matched = vec![false; detections.len()];

        // Try to match each existing track to a detection
        for track in &mut self.tracks {
            let mut best_iou = 0.0f32;
            let mut best_idx = None;

            for (i, det) in detections.iter().enumerate() {
                if det_matched[i] || det.class_id != track.class_id {
                    continue;
                }
                let det_bbox = BBox::from(det);
                let iou = track.bbox.iou(&det_bbox);
                if iou > best_iou && iou >= IOU_MATCH_THRESHOLD {
                    best_iou = iou;
                    best_idx = Some(i);
                }
            }

            if let Some(idx) = best_idx {
                det_matched[idx] = true;
                let det = &detections[idx];
                let new_bbox = BBox::from(det);
                let new_center = new_bbox.center();
                let dx = new_center.0 - track.prev_center.0;
                let dy = new_center.1 - track.prev_center.1;
                track.total_movement += (dx * dx + dy * dy).sqrt();
                track.prev_center = new_center;
                track.bbox = new_bbox;
                track.score = det.score;
                track.last_seen = now;
                track.frames_seen += 1;
            }
        }

        // Create new tracks for unmatched detections
        for (i, det) in detections.iter().enumerate() {
            if det_matched[i] {
                continue;
            }
            let bbox = BBox::from(det);
            let center = bbox.center();
            self.tracks.push(TrackedObject {
                track_id: self.next_id,
                class: det.class.clone(),
                class_id: det.class_id,
                bbox,
                score: det.score,
                first_seen: now,
                last_seen: now,
                frames_seen: 1,
                prev_center: center,
                total_movement: 0.0,
            });
            self.next_id += 1;
        }

        // Remove stale tracks
        self.tracks.retain(|t| now.duration_since(t.last_seen) < MAX_STALE_AGE);

        self.tracks.clone()
    }

    pub fn active_tracks(&self) -> &[TrackedObject] {
        &self.tracks
    }

    /// Check if any tracks were first seen after `since`.
    pub fn has_new_arrivals_since(&self, since: Instant) -> bool {
        self.tracks.iter().any(|t| t.first_seen > since)
    }

    pub fn longest_duration(&self) -> Option<Duration> {
        self.tracks.iter().map(|t| t.duration()).max()
    }
}
