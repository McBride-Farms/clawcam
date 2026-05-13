//! Webhook lifecycle.
//!
//! `continuous` (default): any frame with ≥1 detection above threshold
//! fires a single-phase webhook, throttled to `cooldown` between bursts.
//!
//! `event`: first detection fires a `start` phase with a UUIDv4
//! `event_id`; subsequent detected frames update the timer silently;
//! after `idle` of no detections, fire an `end` phase with the elapsed
//! duration.
//!
//! We deliberately do not assemble clips here. The Pi-side
//! `detect::monitor` already produces the canonical mp4 on its own `end`
//! phase, with pre-roll frames and clip_predictions. Re-doing that off
//! device would duplicate the work and burn extra encode CPU on the
//! worker host for no behavioural gain.

use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::webhook::Detection;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Continuous,
    Event,
}

impl Mode {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "event" | "events" => Mode::Event,
            _ => Mode::Continuous,
        }
    }
}

pub enum Emit<'a> {
    None,
    Single {
        detections: &'a [Detection],
    },
    Start {
        event_id: String,
        detections: &'a [Detection],
    },
    End {
        event_id: String,
        duration_secs: f64,
    },
}

pub struct Lifecycle {
    mode: Mode,
    cooldown: Duration,
    idle: Duration,
    last_emit: Option<Instant>,
    open_event: Option<OpenEvent>,
}

struct OpenEvent {
    id: String,
    started_at: Instant,
    last_seen: Instant,
}

impl Lifecycle {
    pub fn new(mode: Mode, cooldown: Duration, idle: Duration) -> Self {
        Self {
            mode,
            cooldown,
            idle,
            last_emit: None,
            open_event: None,
        }
    }

    pub fn step<'a>(&mut self, detections: &'a [Detection], now: Instant) -> Emit<'a> {
        match self.mode {
            Mode::Continuous => self.step_continuous(detections, now),
            Mode::Event => self.step_event(detections, now),
        }
    }

    fn step_continuous<'a>(&mut self, detections: &'a [Detection], now: Instant) -> Emit<'a> {
        if detections.is_empty() {
            return Emit::None;
        }
        if let Some(last) = self.last_emit
            && now.duration_since(last) < self.cooldown
        {
            return Emit::None;
        }
        self.last_emit = Some(now);
        Emit::Single { detections }
    }

    fn step_event<'a>(&mut self, detections: &'a [Detection], now: Instant) -> Emit<'a> {
        if !detections.is_empty() {
            match &mut self.open_event {
                Some(ev) => {
                    ev.last_seen = now;
                    Emit::None
                }
                None => {
                    let id = Uuid::new_v4().to_string();
                    self.open_event = Some(OpenEvent {
                        id: id.clone(),
                        started_at: now,
                        last_seen: now,
                    });
                    Emit::Start {
                        event_id: id,
                        detections,
                    }
                }
            }
        } else if let Some(ev) = &self.open_event {
            if now.duration_since(ev.last_seen) >= self.idle {
                let id = ev.id.clone();
                let dur = ev.started_at.elapsed().as_secs_f64();
                self.open_event = None;
                return Emit::End {
                    event_id: id,
                    duration_secs: dur,
                };
            }
            Emit::None
        } else {
            Emit::None
        }
    }
}
