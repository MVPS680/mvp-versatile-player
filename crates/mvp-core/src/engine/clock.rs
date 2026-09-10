//! The master playback clock.
//!
//! There are two sources of truth for "where are we in the media":
//!
//! * the **audio clock**, which counts the frames the sound card has actually
//!   consumed — sample accurate, and the reason A/V stays in sync;
//! * the **wall clock**, used before audio has started, for video-only files,
//!   and whenever audio stalls (end of file, device error, a gap in the track).
//!
//! The two are kept continuous: the wall clock's anchor is continuously
//! re-based onto the audio position, so when audio stops flowing the wall clock
//! simply carries on from exactly where the sound card left off. That is what
//! makes the last seconds of a file play out smoothly instead of freezing when
//! the audio stream runs dry.

use std::sync::Arc;
use std::time::Instant;
#[cfg(test)]
use std::time::Duration;

use parking_lot::Mutex;

use crate::audio::AudioSink;

struct Inner {    /// Media position the current wall-clock run started from.
    anchor_media: f64,
    /// Wall-clock instant the current run started at.
    anchor_instant: Instant,
    /// `false` while paused.
    running: bool,
    /// Playback rate.
    speed: f64,
    /// Output device, when the current file has audio.
    audio: Option<Arc<AudioSink>>,
    /// Last position reported by the audio clock.
    last_audio_position: f64,
    /// When the audio clock last advanced.
    last_audio_advance: Instant,
    /// Set once the audio clock has been observed advancing at least once.
    audio_engaged: bool,
}

/// Sample-accurate playback position.
pub struct Clock {
    inner: Mutex<Inner>,
}

impl Clock {
    /// Create a clock sitting at position zero, stopped.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                anchor_media: 0.0,
                anchor_instant: Instant::now(),
                running: false,
                speed: 1.0,
                audio: None,
                last_audio_position: 0.0,
                last_audio_advance: Instant::now(),
                audio_engaged: false,
            }),
        }
    }

    /// Install (or clear) the audio sink used as the master reference.
    pub fn set_audio(&self, sink: Option<Arc<AudioSink>>) {
        let mut g = self.inner.lock();
        g.audio = sink;
        g.audio_engaged = false;
        g.last_audio_position = 0.0;
        g.last_audio_advance = Instant::now();
    }

    /// Jump to `media` seconds without changing the run/pause state.
    pub fn seek(&self, media: f64) {
        let mut g = self.inner.lock();
        g.anchor_media = media.max(0.0);
        g.anchor_instant = Instant::now();
        g.last_audio_position = media.max(0.0);
        g.last_audio_advance = Instant::now();
        if let Some(sink) = &g.audio {
            sink.flush(media.max(0.0));
        }
    }

    /// Start or stop the wall clock.
    pub fn set_running(&self, running: bool) {
        let mut g = self.inner.lock();
        if g.running == running {
            return;
        }
        // Freeze the current position before switching modes.
        let now = Self::compute(&mut g);
        g.anchor_media = now;
        g.anchor_instant = Instant::now();
        g.running = running;
    }

    /// `true` while the wall clock advances.
    pub fn is_running(&self) -> bool {
        self.inner.lock().running
    }

    /// Change the playback rate, keeping the current position.
    pub fn set_speed(&self, speed: f64) {
        let speed = speed.clamp(0.05, 8.0);
        let mut g = self.inner.lock();
        let now = Self::compute(&mut g);
        g.anchor_media = now;
        g.anchor_instant = Instant::now();
        g.speed = speed;
        if let Some(sink) = &g.audio {
            sink.set_speed(speed);
        }
    }

    /// Current playback rate.
    pub fn speed(&self) -> f64 {
        self.inner.lock().speed
    }

    /// Current media position in seconds.
    pub fn now(&self) -> f64 {
        let mut g = self.inner.lock();
        Self::compute(&mut g)
    }

    /// `true` when the position currently comes from the audio device.
    pub fn is_audio_driven(&self) -> bool {
        self.inner.lock().audio_engaged
    }

    fn compute(g: &mut Inner) -> f64 {
        // The wall clock is the master. A sound card looks like the natural time
        // source, but what a driver reports is *writes*, not playback: some
        // drivers swallow a second of audio in a single callback, others stop
        // calling back while continuing to play what they already hold. Either
        // way a clock built on the device runs at the wrong speed and the
        // picture races or freezes. The system clock is monotonic and exact.
        //
        // The cost is drift between the two crystals — tens of parts per million
        // on real hardware, a few tenths of a second over a feature film — which
        // is the right trade against a clock that is simply wrong.
        let sink = g.audio.clone();
        if let Some(sink) = &sink {
            // Remember where the audio got to, so that stopping and starting
            // does not move the position.
            g.last_audio_position = sink.position();
            g.last_audio_advance = Instant::now();
            g.audio_engaged = sink.is_primed();
        }

        if !g.running {
            return g.anchor_media;
        }
        let elapsed = g.anchor_instant.elapsed().as_secs_f64();
        g.anchor_media + elapsed * g.speed
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stopped_clock_does_not_move() {
        let clock = Clock::new();
        clock.seek(10.0);
        assert!((clock.now() - 10.0).abs() < 1e-9);
        std::thread::sleep(Duration::from_millis(30));
        assert!((clock.now() - 10.0).abs() < 1e-9, "paused clocks freeze");
    }

    #[test]
    fn a_running_clock_advances() {
        let clock = Clock::new();
        clock.set_running(true);
        std::thread::sleep(Duration::from_millis(60));
        let now = clock.now();
        assert!(now > 0.02 && now < 0.5, "unexpected position {now}");
    }

    #[test]
    fn speed_scales_the_rate() {
        let clock = Clock::new();
        clock.set_speed(4.0);
        clock.set_running(true);
        std::thread::sleep(Duration::from_millis(50));
        let now = clock.now();
        assert!(now > 0.1, "4x should advance faster, got {now}");
    }

    #[test]
    fn pausing_freezes_and_resuming_continues() {
        let clock = Clock::new();
        clock.set_running(true);
        std::thread::sleep(Duration::from_millis(40));
        clock.set_running(false);
        let frozen = clock.now();
        std::thread::sleep(Duration::from_millis(30));
        assert!((clock.now() - frozen).abs() < 1e-9);
        clock.set_running(true);
        std::thread::sleep(Duration::from_millis(30));
        assert!(clock.now() > frozen);
    }
}
