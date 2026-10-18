//! Sending Backtalk's own audio from the phone, with AudioNet's sender.
//!
//! The app hands over what Backtalk's low-latency mixer plays, as it mixes
//! it. That timing is not real time: the mixer runs only while there is
//! speech or a sound, and while it runs it writes ahead into its track in
//! bursts, then waits. AudioNet's sender wants a steady capture, so a
//! [`Reclock`] in between hands it exactly the audio due by the clock: the
//! mixer's bursts wait in a short queue, and silence fills in only when the
//! queue has stayed empty past a small allowance. The sender thread
//! (AudioNet's) encodes 10 ms Opus frames, seals them with the stream's
//! media key and sends them to the relay.
//!
//! Measured through the relay before the reclock (Hypothesis, Evidence):
//! adding silence whenever the mixer paused for 60 ms emptied the
//! controller's 60 ms playout buffer at the end of each sound (12 underruns
//! in 14 seconds); at 25 ms, silence overlapped the mixer's bursts, so the
//! stream ran faster than real time (119 packets a second), the playout
//! depth reached 251 ms and was trimmed 7 times (skips).

use std::collections::VecDeque;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use audionet_audio::ring::{RingProducer, audio_ring};
use audionet_codec::EncoderConfig;
use audionet_engine::runtime::SenderRuntime;
use audionet_transport::crypto::PresharedKey;

/// How often the reclock hands audio to the sender.
const PACE: Duration = Duration::from_millis(5);
/// Capture ring length: AudioNet's sender trims anything older than
/// 200 ms, so a second only absorbs scheduling stalls.
const RING_SECONDS: usize = 1;

/// Turns audio that comes in bursts and gaps into a steady stream at the
/// clock's rate. Sans-I/O: the caller gives the time.
#[derive(Debug)]
pub struct Reclock {
    rate: u32,
    /// Interleaved stereo waiting to be due.
    queue: VecDeque<f32>,
    start_ns: u64,
    sent_frames: u64,
    /// Frames due but not sent, while the queue is empty, before silence.
    max_debt_frames: u64,
    /// Above this many frames queued, the oldest go, down to `keep_frames`.
    high_frames: usize,
    keep_frames: usize,
    pub stats: Arc<ReclockStats>,
}

/// What the reclock did, by cause.
#[derive(Debug, Default)]
pub struct ReclockStats {
    /// Silence added because no audio was there when it was due.
    pub silence_frames: AtomicU64,
    /// Audio dropped because too much was queued (it came faster than real
    /// time for long).
    pub dropped_frames: AtomicU64,
}

impl Reclock {
    /// A stream at `rate` starting at `now_ns`.
    pub fn new(rate: u32, now_ns: u64) -> Self {
        let ms = |n: u64| n * u64::from(rate) / 1000;
        Self {
            rate,
            queue: VecDeque::with_capacity(rate as usize),
            start_ns: now_ns,
            sent_frames: 0,
            // Under the controller's playout target (60 ms to start), so
            // the end of a sound never runs it dry, and longer than the
            // app's hand-over period (10 ms) plus its thread's lateness.
            max_debt_frames: ms(30),
            // The mixer writes ahead by about its track's buffer: 40 ms
            // without the fast path. More than this is a clock running
            // fast, or a stall.
            high_frames: ms(150) as usize,
            keep_frames: ms(40) as usize,
            stats: Arc::new(ReclockStats::default()),
        }
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Queues interleaved stereo samples.
    pub fn push(&mut self, samples: &[f32]) {
        self.queue.extend(samples);
    }

    /// Puts in `out` (cleared first) the frames due by `now_ns`: queued
    /// audio, then silence only if the queue has been short for longer
    /// than the allowance.
    pub fn take(&mut self, now_ns: u64, out: &mut Vec<f32>) {
        out.clear();
        let elapsed = now_ns.saturating_sub(self.start_ns);
        let due_total = (u128::from(elapsed) * u128::from(self.rate) / 1_000_000_000) as u64;
        let due = due_total.saturating_sub(self.sent_frames);
        let mut queued = self.queue.len() / 2;
        if queued > self.high_frames {
            let drop = queued - self.keep_frames;
            self.queue.drain(..drop * 2);
            self.stats.dropped_frames.fetch_add(drop as u64, Relaxed);
            queued = self.keep_frames;
        }
        let audio = (due as usize).min(queued);
        out.extend(self.queue.drain(..audio * 2));
        self.sent_frames += audio as u64;
        let debt = due - audio as u64;
        if debt > self.max_debt_frames {
            out.resize(out.len() + debt as usize * 2, 0.0);
            self.sent_frames += debt;
            self.stats.silence_frames.fetch_add(debt, Relaxed);
        }
    }
}

struct Feed {
    producer: RingProducer,
    reclock: Reclock,
    /// Kept so the sender thread runs; stopped when the feed is dropped.
    _sender: SenderRuntime,
    scratch: Vec<f32>,
}

/// The phone's audio stream to its controllers.
pub struct HostAudio {
    feed: Arc<Mutex<Option<Feed>>>,
    socket: UdpSocket,
    relay: SocketAddr,
    key: PresharedKey,
    stop: Arc<AtomicBool>,
    pacer: Option<JoinHandle<()>>,
    epoch: Instant,
}

impl std::fmt::Debug for HostAudio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostAudio")
            .field("relay", &self.relay)
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn since(epoch: Instant) -> u64 {
    epoch.elapsed().as_nanos() as u64
}

impl HostAudio {
    /// Sends to `relay` from `socket` with the media key `key`. Nothing is
    /// sent until the first [`push`](Self::push).
    pub fn start(socket: UdpSocket, relay: SocketAddr, key: [u8; 32]) -> Result<HostAudio, String> {
        let feed: Arc<Mutex<Option<Feed>>> = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let epoch = Instant::now();
        let pacer = {
            let feed = Arc::clone(&feed);
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("remote-audio-pacer".into())
                .spawn(move || {
                    let mut out = Vec::new();
                    while !stop.load(Relaxed) {
                        std::thread::sleep(PACE);
                        if let Some(feed) = lock(&feed).as_mut() {
                            feed.reclock.take(since(epoch), &mut out);
                            // What does not fit is dropped and counted by
                            // AudioNet's ring.
                            let _ = feed.producer.write(&out);
                        }
                    }
                })
                .map_err(|e| format!("Could not start the audio pacer: {e}"))?
        };
        Ok(HostAudio {
            feed,
            socket,
            relay,
            key: PresharedKey::from_bytes(key),
            stop,
            pacer: Some(pacer),
            epoch,
        })
    }

    /// Adds `samples`: interleaved stereo at `rate`, as 32-bit floats in
    /// little-endian order. Never waits on the network.
    pub fn push(&self, rate: u32, samples: &[u8]) -> Result<(), String> {
        if !(8_000..=192_000).contains(&rate) || samples.len() % 8 != 0 {
            return Err("Audio must be stereo 32-bit samples at 8 to 192 kHz.".into());
        }
        let mut slot = lock(&self.feed);
        if slot.as_ref().is_none_or(|f| f.reclock.rate() != rate) {
            // The first audio, or the mixer's rate changed (a new output):
            // a new stream at the new rate.
            *slot = None;
            *slot = Some(self.new_feed(rate)?);
        }
        let feed = slot.as_mut().expect("just made");
        feed.scratch.clear();
        feed.scratch.extend(
            samples
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        );
        feed.reclock.push(&feed.scratch);
        Ok(())
    }

    fn new_feed(&self, rate: u32) -> Result<Feed, String> {
        let (producer, consumer) = audio_ring(rate as usize * RING_SECONDS, 2);
        let socket = self
            .socket
            .try_clone()
            .map_err(|e| format!("Could not share the audio socket: {e}"))?;
        let sender = SenderRuntime::start(
            consumer,
            rate,
            socket,
            Arc::new(Mutex::new(self.relay)),
            &self.key,
            EncoderConfig::BASELINE,
            None,
        )
        .map_err(|e| format!("Could not start sending audio: {e}"))?;
        Ok(Feed {
            producer,
            reclock: Reclock::new(rate, since(self.epoch)),
            _sender: sender,
            scratch: Vec::with_capacity(rate as usize / 10),
        })
    }
}

impl Drop for HostAudio {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(t) = self.pacer.take() {
            let _ = t.join();
        }
        // Stops AudioNet's sender thread.
        lock(&self.feed).take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    const MS: u64 = 1_000_000;

    fn frames(n: usize, value: f32) -> Vec<f32> {
        vec![value; n * 2]
    }

    /// Runs the reclock every 5 ms from `from` to `to` (milliseconds),
    /// returning all it gave out.
    fn run(r: &mut Reclock, from: u64, to: u64, out_all: &mut Vec<f32>) {
        let mut out = Vec::new();
        let mut t = from;
        while t <= to {
            r.take(t * MS, &mut out);
            out_all.extend_from_slice(&out);
            t += 5;
        }
    }

    #[test]
    fn bursts_come_out_at_the_clock_rate() {
        let mut r = Reclock::new(RATE, 0);
        let mut all = Vec::new();
        // 40 ms written at once, then 10 ms every 10 ms: a mixer filling
        // its track's buffer, then keeping it full.
        r.push(&frames(1920, 0.5));
        for step in 0..100u64 {
            r.push(&frames(480, 0.5));
            run(&mut r, step * 10, step * 10 + 5, &mut all);
        }
        // A second of audio came out in a second, none of it silence.
        let out = all.len() / 2;
        assert!((47_000..=48_500).contains(&out), "{out} frames");
        assert_eq!(r.stats.silence_frames.load(Relaxed), 0);
        assert_eq!(r.stats.dropped_frames.load(Relaxed), 0);
        assert!(all.iter().all(|&s| s == 0.5));
    }

    #[test]
    fn silence_fills_in_only_after_the_allowance() {
        let mut r = Reclock::new(RATE, 0);
        let mut all = Vec::new();
        r.push(&frames(480, 0.5));
        run(&mut r, 0, 10, &mut all);
        // 10 ms of audio, then 20 ms of nothing: still within the allowance.
        run(&mut r, 15, 30, &mut all);
        assert_eq!(r.stats.silence_frames.load(Relaxed), 0);
        assert_eq!(all.len() / 2, 480);
        // Past it, silence keeps the stream at the clock's rate.
        run(&mut r, 35, 1000, &mut all);
        let out = all.len() / 2;
        assert!((46_000..=48_000).contains(&out), "{out} frames");
        assert!(r.stats.silence_frames.load(Relaxed) > 40_000);
    }

    #[test]
    fn a_slow_mixer_clock_never_puts_silence_inside_a_sound() {
        let mut r = Reclock::new(RATE, 0);
        let mut all = Vec::new();
        // 10 s of a mixer running 200 ppm slow: 479.904 frames per 10 ms.
        let mut owed = 0.0f64;
        for step in 0..1000u64 {
            owed += 479.904;
            let n = owed as usize;
            owed -= n as f64;
            r.push(&frames(n, 0.5));
            run(&mut r, step * 10, step * 10 + 5, &mut all);
        }
        assert_eq!(r.stats.silence_frames.load(Relaxed), 0);
        assert!(all.iter().all(|&s| s == 0.5));
    }

    #[test]
    fn a_fast_or_stalled_source_is_trimmed() {
        let mut r = Reclock::new(RATE, 0);
        let mut out = Vec::new();
        // 300 ms arrive at once.
        r.push(&frames(14_400, 0.5));
        r.take(5 * MS, &mut out);
        assert!(r.stats.dropped_frames.load(Relaxed) > 0);
        assert!(r.queue.len() / 2 <= r.keep_frames);
    }
}
