use std::time::{Duration, Instant};

use crate::detect::tracker::TrackedObject;

/// What the event manager decided the monitor should do.
pub enum EventDecision {
    /// Nothing to report.
    Quiet,

    /// First detection — fire an initial alert with pre-detection frames.
    InitialAlert {
        tracks: Vec<TrackedObject>,
    },

    /// Something changed during an active event (new arrival, prolonged).
    Update {
        tracks: Vec<TrackedObject>,
        reason: UpdateReason,
    },

    /// Event is over — send final report, optionally with clip data.
    Complete {
        total_duration: Duration,
    },
}

#[derive(Debug, Clone)]
pub enum UpdateReason {
    NewArrival,
    Prolonged,
}

enum State {
    Idle,
    Active {
        started: Instant,
        sent_initial: bool,
        sent_prolonged: bool,
    },
    Cooldown {
        started: Instant,
        event_started: Instant,
    },
}

// How long objects must be gone before we close the event.
const DEPARTURE_GRACE: Duration = Duration::from_secs(3);
// How long an event must last before we send a "prolonged" update.
const PROLONGED_THRESHOLD: Duration = Duration::from_secs(3);
// Minimum gap between update webhooks to avoid spam.
const MIN_UPDATE_INTERVAL: Duration = Duration::from_secs(10);

pub struct EventManager {
    state: State,
    last_report: Instant,
}

impl EventManager {
    pub fn new() -> Self {
        Self {
            state: State::Idle,
            last_report: Instant::now() - MIN_UPDATE_INTERVAL,
        }
    }

    /// Returns true if currently in an active event (Active or Cooldown).
    pub fn is_recording(&self) -> bool {
        !matches!(self.state, State::Idle)
    }

    pub fn event_start(&self) -> Option<Instant> {
        match &self.state {
            State::Active { started, .. } => Some(*started),
            State::Cooldown { event_started, .. } => Some(*event_started),
            State::Idle => None,
        }
    }

    pub fn evaluate(
        &mut self,
        tracks: &[TrackedObject],
        has_new_arrivals: bool,
    ) -> EventDecision {
        let now = Instant::now();
        let has_objects = !tracks.is_empty();

        match &mut self.state {
            State::Idle => {
                if has_objects {
                    self.state = State::Active {
                        started: now,
                        sent_initial: false,
                        sent_prolonged: false,
                    };
                    // Fall through to Active handling below
                } else {
                    return EventDecision::Quiet;
                }
            }
            _ => {}
        }

        match &mut self.state {
            State::Active { started, sent_initial, sent_prolonged } => {
                if !*sent_initial {
                    *sent_initial = true;
                    self.last_report = now;
                    return EventDecision::InitialAlert {
                        tracks: tracks.to_vec(),
                    };
                }

                if !has_objects {
                    let event_started = *started;
                    self.state = State::Cooldown {
                        started: now,
                        event_started,
                    };
                    return EventDecision::Quiet;
                }

                // Check for prolonged presence
                if !*sent_prolonged
                    && now.duration_since(*started) >= PROLONGED_THRESHOLD
                    && now.duration_since(self.last_report) >= MIN_UPDATE_INTERVAL
                {
                    *sent_prolonged = true;
                    self.last_report = now;
                    return EventDecision::Update {
                        tracks: tracks.to_vec(),
                        reason: UpdateReason::Prolonged,
                    };
                }

                // Check for new arrivals
                if has_new_arrivals
                    && now.duration_since(self.last_report) >= MIN_UPDATE_INTERVAL
                {
                    self.last_report = now;
                    return EventDecision::Update {
                        tracks: tracks.to_vec(),
                        reason: UpdateReason::NewArrival,
                    };
                }

                EventDecision::Quiet
            }

            State::Cooldown { started, event_started } => {
                if has_objects {
                    // Objects reappeared — go back to Active
                    let evt_start = *event_started;
                    self.state = State::Active {
                        started: evt_start,
                        sent_initial: true,
                        sent_prolonged: true,
                    };
                    return EventDecision::Quiet;
                }

                if now.duration_since(*started) >= DEPARTURE_GRACE {
                    let total = now.duration_since(*event_started);
                    self.state = State::Idle;
                    return EventDecision::Complete {
                        total_duration: total,
                    };
                }

                EventDecision::Quiet
            }

            State::Idle => unreachable!(),
        }
    }
}
