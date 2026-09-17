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
    /// The container has no more packets **for this decode session**.
    ///
    /// Tagged like a packet, and for the same reason: a read that a *newer*
    /// seek interrupts reports the end of the stream exactly like the end of
    /// the file does, so this message can easily be the last thing in the
    /// channel when the next session starts. Acting on it after the flush has
    /// already put the decoder into its new state leaves that decoder drained —
    /// `send_eof` is answered by every later packet with `AVERROR_EOF` — and the
    /// picture never comes back, no matter how many times the user seeks.
    Eof {
        /// Decode session the end belongs to.
        generation: u64,
    },
    /// Stop the worker as soon as possible.
    Stop,
}

/// Control messages for the audio worker. See [`VideoMsg`] for why seeks and
/// stream changes are not here.
pub enum AudioMsg {
    /// Decode this packet.
    Packet(PacketMsg),
    /// The container has no more packets **for this decode session**. See
    /// [`VideoMsg::Eof`] for why the session is part of the message.
    Eof {
        /// Decode session the end belongs to.
        generation: u64,
    },
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
    /// How far ahead of the playhead the queue may run, in seconds of media.
    /// `0.0` means "no time bound", which is what [`VideoQueue::new`] gives.
    time_budget: f64,
    /// Source frame rate the time budget is turned into a frame count with.
    /// `0.0` until [`VideoQueue::set_pacing`] is told.
    fps: f64,
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
            time_budget: 0.0,
            fps: 0.0,
            min_frames: 2,
            max_frames: max_frames.max(3),
        }
    }

    /// A queue that also refuses to hold more than `seconds` of media.
    ///
    /// The byte budget alone describes memory, not *lag*: a 1080p queue will
    /// happily accept sixteen frames, which is half a second of material the
    /// decoder has already read and the playhead has not reached. Half a second
    /// of lead is half a second of delay before a seek, a speed change or a
    /// pause can take effect, and it is paid for nothing — a queue only ever
    /// needs to bridge one screen's worth of jitter.
    ///
    /// Frame size varies enormously (8 MB at 1080p, 132 MB at 8K), so the frame
    /// count that fits a given number of seconds is derived from the source's
    /// frame rate by [`Self::set_pacing`].
    pub fn with_time_budget(budget_bytes: usize, max_frames: usize, seconds: f64) -> Self {
        Self {
            time_budget: if seconds.is_finite() && seconds > 0.0 {
                seconds
            } else {
                0.0
            },
            ..Self::new(budget_bytes, max_frames)
        }
    }

    /// Tell the queue how fast the source runs, so the time budget can be
    /// turned into a frame count.
    pub fn set_pacing(&mut self, fps: f64) {
        if fps.is_finite() && fps > 0.0 {
            self.fps = fps;
        }
    }

    /// Number of frames this queue may hold, given the time budget and the
    /// source frame rate.
    ///
    /// Never above `max_frames` and never below `min_frames`: a budget that
    /// rounded down to one frame would not be a buffer at all, and the byte
    /// budget already has its own floor for the same reason.
    fn frame_cap(&self) -> usize {
        if self.time_budget <= 0.0 || self.fps <= 0.0 {
            return self.max_frames;
        }
        let by_time = (self.fps * self.time_budget).ceil();
        let by_time = by_time.clamp(self.min_frames as f64, self.max_frames as f64);
        by_time as usize
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
        self.frames.len() >= self.frame_cap()
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
        self.take_ready_counted(now, tolerance).0
    }

    /// [`Self::take_ready`], and how many frames it threw away.
    ///
    /// The count is what tells "the decoder cannot keep up" apart from "the
    /// decoder is converting frames the screen was never going to show": at
    /// 120 fps on a 60 Hz display every other frame is superseded before it can
    /// be presented. Dropping them silently is why those two cases looked
    /// identical in the statistics panel.
    pub fn take_ready_counted(
        &mut self,
        now: f64,
        tolerance: f64,
    ) -> (Option<Arc<VideoFrame>>, u64) {
        let mut chosen: Option<Arc<VideoFrame>> = None;
        let mut discarded = 0u64;
        while let Some(front) = self.frames.front() {
            if front.pts > now + tolerance {
                break;
            }
            if chosen.is_some() {
                discarded += 1;
            }
            chosen = self.pop_front();
        }
        (chosen, discarded)
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
    fn take_ready_counts_the_frames_it_supersedes() {
        // The count is what tells "the decoder cannot keep up" apart from "the
        // decoder is converting frames a faster-than-screen source produced".
        // Dropping them silently is why the two looked identical.
        let mut q = VideoQueue::new(64 * 1024 * 1024, 16);
        for i in 0..10 {
            push(&mut q, frame(i as f64 * 0.04, 1024));
        }
        let (got, discarded) = q.take_ready_counted(0.10, 0.02);
        let got = got.expect("a frame must be ready");
        assert!((got.pts - 0.12).abs() < 1e-9, "got {}", got.pts);
        assert_eq!(discarded, 3, "three frames were superseded by the one shown");
        assert_eq!(q.len(), 6);
    }

    #[test]
    fn take_ready_counts_nothing_when_only_one_frame_is_due() {
        let mut q = VideoQueue::new(64 * 1024 * 1024, 16);
        push(&mut q, frame(0.0, 1024));
        push(&mut q, frame(1.0, 1024));
        let (got, discarded) = q.take_ready_counted(0.01, 0.0);
        assert!(got.is_some());
        assert_eq!(discarded, 0, "a frame that is shown was not a drop");
        assert_eq!(q.len(), 1);
        // And asking again with nothing due reports nothing at all.
        let (none, discarded) = q.take_ready_counted(0.02, 0.0);
        assert!(none.is_none());
        assert_eq!(discarded, 0);
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
    fn a_time_budget_bounds_how_far_ahead_the_decoder_may_run() {
        // Small frames and a generous byte budget: without a time bound the
        // queue would accept all sixteen. At 24 fps a quarter of a second is
        // six frames, which is the *lag* limit — bytes alone say nothing about
        // how much material has been read ahead of the playhead.
        let mut q = VideoQueue::with_time_budget(64 * 1024 * 1024, 16, 0.25);
        q.set_pacing(24.0);
        let mut accepted = 0;
        for i in 0..32 {
            if q.try_push(frame(i as f64 * (1.0 / 24.0), 1024)).is_none() {
                accepted += 1;
            }
        }
        assert_eq!(accepted, 6, "a quarter of a second at 24 fps");
        assert!(q.is_full());
    }

    #[test]
    fn a_time_budget_never_shrinks_the_queue_below_a_usable_buffer() {
        // One frame is not a buffer: a queue sized by a very high frame rate or
        // a very short budget must still hold a couple of frames, or playback
        // would stall on the first hiccup.
        let mut q = VideoQueue::with_time_budget(64 * 1024 * 1024, 16, 0.0001);
        q.set_pacing(1000.0);
        let mut accepted = 0;
        for i in 0..8 {
            if q.try_push(frame(i as f64, 1024)).is_none() {
                accepted += 1;
            }
        }
        assert_eq!(accepted, 2, "the floor is a real buffer, not one frame");
    }

    #[test]
    fn an_unpaced_queue_behaves_exactly_as_before() {
        // `VideoQueue::new` takes no pacing, and a source whose frame rate is
        // unknown must be treated as \"no time bound\" rather than as zero
        // frames.
        let mut q = VideoQueue::new(64 * 1024 * 1024, 4);
        for i in 0..4 {
            push(&mut q, frame(i as f64, 1024));
        }
        assert!(q.is_full());
        // A nonsensical frame rate must not change anything either.
        let mut q = VideoQueue::with_time_budget(64 * 1024 * 1024, 4, 0.25);
        q.set_pacing(f64::NAN);
        q.set_pacing(0.0);
        for i in 0..4 {
            push(&mut q, frame(i as f64, 1024));
        }
        assert!(q.is_full());
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
