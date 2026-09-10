//! Bounded video frame queue and the message types that feed the decoder
//! worker threads.

use std::collections::VecDeque;
use std::sync::Arc;

use ffmpeg_next as ffmpeg;

use crate::video::VideoFrame;

/// One demuxed packet tagged with the decode generation it belongs to.
///
/// A seek bumps the generation; workers silently discard packets carrying an
/// older one, which is how stale data from before the seek is prevented from
/// reaching the screen without any synchronisation handshake.
pub struct PacketMsg {
    /// The demuxed packet.
    pub packet: ffmpeg::Packet,
    /// Decode session this packet belongs to.
    pub generation: u64,
    /// Packet timestamp in seconds, `None` when the container did not supply one.
    pub timestamp: Option<f64>,
}

/// Control messages for the video worker.
///
/// Only *data* and end-of-stream travel through the channel. Seek and
/// track-change requests go through shared state instead (see
/// [`super::Shared::request_flush`]) because a control message must never queue
/// behind a full backlog of packets: the worker would keep decoding stale frames
/// while the demuxer blocked trying to tell it to stop.
pub enum VideoMsg {
    /// Decode this packet.
    Packet(PacketMsg),
    /// The container has no more packets.
    Eof,
    /// Stop the worker as soon as possible.
    Stop,
}

/// Control messages for the audio worker. See [`VideoMsg`] for why seeks and
/// stream changes are not here.
pub enum AudioMsg {
    /// Decode this packet.
    Packet(PacketMsg),
    /// The container has no more packets.
    Eof,
    /// Stop the worker as soon as possible.
    Stop,
}

/// A byte-budgeted FIFO of decoded frames.
///
/// The budget matters: a 4K RGBA frame is 33 MB, so a naive "keep 16 frames"
/// queue would reserve half a gigabyte. Capacity therefore adapts — always at
/// least two frames so playback can continue, never so many that memory runs
/// away.
#[derive(Debug)]
pub struct VideoQueue {
    frames: VecDeque<Arc<VideoFrame>>,
    bytes: usize,
    budget_bytes: usize,
    min_frames: usize,
    max_frames: usize,
}

impl VideoQueue {
    /// Create a queue holding between 2 and `max_frames` frames, and never more
    /// than `budget_bytes` of pixel data.
    pub fn new(budget_bytes: usize, max_frames: usize) -> Self {
        Self {
            frames: VecDeque::new(),
            bytes: 0,
            budget_bytes: budget_bytes.max(1024 * 1024),
            min_frames: 2,
            max_frames: max_frames.max(3),
        }
    }

    /// Number of frames waiting.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// `true` when nothing is queued.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Pixel bytes currently held.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// `true` when another frame would exceed the budget.
    pub fn is_full(&self) -> bool {
        self.frames.len() >= self.max_frames
            || (self.frames.len() >= self.min_frames && self.bytes >= self.budget_bytes)
    }

    /// Presentation timestamp of the oldest frame.
    pub fn front_pts(&self) -> Option<f64> {
        self.frames.front().map(|f| f.pts)
    }

    /// Presentation timestamp of the newest frame.
    pub fn back_pts(&self) -> Option<f64> {
        self.frames.back().map(|f| f.pts)
    }

    /// Offer a frame to the queue.
    ///
    /// Returns `None` when the frame was accepted, or the frame itself when
    /// there is no room. **A full queue never discards an older frame.** The
    /// queue is a *presentation* buffer, so the oldest frame is precisely the
    /// one the interface needs next: evicting it to make room for a later
    /// frame leaves the queue holding nothing but future timestamps, and the
    /// consumer — which only ever shows frames that are due — then waits
    /// forever behind a queue that looks full but holds no playable frame at
    /// all. Callers must therefore wait for room instead of forcing a push.
    pub fn try_push(&mut self, frame: Arc<VideoFrame>) -> Option<Arc<VideoFrame>> {
        if self.is_full() {
            return Some(frame);
        }
        self.bytes += frame.memory_cost();
        self.frames.push_back(frame);
        None
    }

    /// Remove and return the oldest frame.
    pub fn pop_front(&mut self) -> Option<Arc<VideoFrame>> {
        let frame = self.frames.pop_front()?;
        self.bytes = self.bytes.saturating_sub(frame.memory_cost());
        Some(frame)
    }

    /// Discard everything.
    pub fn clear(&mut self) {
        self.frames.clear();
        self.bytes = 0;
    }

    /// Pop every frame whose timestamp is at or before `now + tolerance`,
    /// returning the newest of them.
    ///
    /// Frames older than the one returned are dropped, which is exactly the
    /// behaviour you want when the decoder has fallen behind: skip ahead rather
    /// than fall further behind.
    pub fn take_ready(&mut self, now: f64, tolerance: f64) -> Option<Arc<VideoFrame>> {
        let mut chosen: Option<Arc<VideoFrame>> = None;
        while let Some(front) = self.frames.front() {
            if front.pts <= now + tolerance {
                chosen = self.pop_front();
            } else {
                break;
            }
        }
        chosen
    }

    /// Seconds until the oldest queued frame should be shown, or `None` when
    /// the queue is empty.
    pub fn time_until_next(&self, now: f64) -> Option<f64> {
        self.frames.front().map(|f| (f.pts - now).max(0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(pts: f64, bytes: usize) -> Arc<VideoFrame> {
        Arc::new(VideoFrame {
            width: 16,
            height: 16,
            pts,
            duration: 1.0 / 25.0,
            data: vec![0u8; bytes],
            serial: 0,
            generation: 0,
        })
    }

    /// Push or panic: the tests below always size the queue to fit.
    fn push(q: &mut VideoQueue, f: Arc<VideoFrame>) {
        assert!(q.try_push(f).is_none(), "queue was unexpectedly full");
    }

    #[test]
    fn take_ready_returns_the_newest_frame_at_or_before_now() {
        let mut q = VideoQueue::new(64 * 1024 * 1024, 16);
        for i in 0..10 {
            push(&mut q, frame(i as f64 * 0.04, 1024));
        }
        let got = q.take_ready(0.10, 0.02).expect("a frame must be ready");
        assert!((got.pts - 0.12).abs() < 1e-9, "got {}", got.pts);
        assert_eq!(q.len(), 6, "four frames should have been consumed");
    }

    #[test]
    fn take_ready_returns_nothing_when_the_next_frame_is_in_the_future() {
        let mut q = VideoQueue::new(64 * 1024 * 1024, 16);
        push(&mut q, frame(5.0, 1024));
        assert!(q.take_ready(1.0, 0.02).is_none());
        assert_eq!(q.len(), 1, "the frame must stay queued");
    }

    #[test]
    fn a_full_queue_hands_the_frame_back_instead_of_eating_an_old_one() {
        // The regression this guards: the queue used to drop its oldest frame
        // to make room, which left it holding nothing but future timestamps —
        // the interface showed one frame and then froze forever.
        let mut q = VideoQueue::new(64 * 1024 * 1024, 4);
        for i in 0..4 {
            push(&mut q, frame(i as f64, 1024));
        }
        assert_eq!(q.len(), 4);
        // The *oldest* frames are the ones kept — they are the ones due next.
        assert_eq!(q.front_pts(), Some(0.0));
        assert_eq!(q.back_pts(), Some(3.0));
        // And the next frame is refused rather than silently swallowing.
        let extra = frame(4.0, 1024);
        let refused = q.try_push(Arc::clone(&extra)).expect("must be refused");
        assert!(Arc::ptr_eq(&refused, &extra), "the frame comes back intact");
        assert_eq!(q.back_pts(), Some(3.0), "the queue is unchanged");
        // Once the interface consumes a frame there is room again.
        assert!(q.pop_front().is_some());
        assert!(q.try_push(frame(4.0, 1024)).is_none());
    }

    #[test]
    fn a_full_queue_never_exceeds_the_byte_budget() {
        // 4 MiB budget with 1 MiB frames: only a handful may be held.
        let mut q = VideoQueue::new(4 * 1024 * 1024, 64);
        let mut refused = 0;
        for i in 0..20 {
            if q.try_push(frame(i as f64, 1024 * 1024)).is_some() {
                refused += 1;
            }
        }
        assert!(q.bytes() <= 4 * 1024 * 1024, "held {} bytes", q.bytes());
        assert!(q.len() >= 2, "the queue must always keep a couple of frames");
        assert!(refused > 0, "the queue must refuse frames once it is full");
    }

    #[test]
    fn clearing_releases_the_accounting() {
        let mut q = VideoQueue::new(64 * 1024 * 1024, 16);
        for i in 0..5 {
            push(&mut q, frame(i as f64, 4096));
        }
        q.clear();
        assert!(q.is_empty());
        assert_eq!(q.bytes(), 0);
    }

    #[test]
    fn time_until_next_is_clamped_at_zero() {
        let mut q = VideoQueue::new(64 * 1024 * 1024, 16);
        push(&mut q, frame(1.0, 16));
        assert_eq!(q.time_until_next(0.0), Some(1.0));
        assert_eq!(q.time_until_next(2.0), Some(0.0));
        assert_eq!(q.time_until_next(0.0).map(|_| ()), Some(()));
        q.clear();
        assert_eq!(q.time_until_next(0.0), None);
    }
}
