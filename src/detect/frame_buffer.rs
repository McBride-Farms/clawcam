use std::collections::VecDeque;

#[derive(Clone)]
pub struct TimestampedFrame {
    pub jpeg: Vec<u8>,
}

/// Fixed-size ring buffer of timestamped JPEG frames.
/// Default capacity 30 = ~3 seconds at 10 FPS, ~1.5 MB.
pub struct FrameBuffer {
    buffer: VecDeque<TimestampedFrame>,
    capacity: usize,
}

impl FrameBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            buffer: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn push(&mut self, jpeg: Vec<u8>) {
        if self.buffer.len() >= self.capacity {
            self.buffer.pop_front();
        }
        self.buffer.push_back(TimestampedFrame { jpeg });
    }

    /// Get the last `n` frames (pre-detection context).
    pub fn recent(&self, n: usize) -> Vec<&TimestampedFrame> {
        let start = self.buffer.len().saturating_sub(n);
        self.buffer.range(start..).collect()
    }

    /// Clone the last `n` frames for clip assembly.
    pub fn clone_recent(&self, n: usize) -> Vec<TimestampedFrame> {
        let start = self.buffer.len().saturating_sub(n);
        self.buffer.range(start..).cloned().collect()
    }
}
