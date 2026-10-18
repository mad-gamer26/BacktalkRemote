//! Playing the phone's audio on the controlling device: AudioNet's receiver
//! (reordering, Opus decoding with loss concealment) and playout (adaptive
//! jitter buffer, clock-drift and depth control) into the default output,
//! through cpal (AAudio on Android, Core Audio on iOS), as AudioNet's own
//! apps play.

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use audionet_audio::clock;
use audionet_audio::render::RenderSource;
use audionet_engine::adaptive::AdaptiveConfig;
use audionet_engine::playout::{PlayoutConfig, PlayoutStats};
use audionet_engine::receiver::{ReceiverConfig, ReceiverStats};
use audionet_engine::runtime::{PlayoutSource, ReceiverRuntime};
use audionet_transport::crypto::PresharedKey;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::link::{HelloSender, Udp};

/// The playout target to start from on the internet, and how it adapts:
/// AudioNet's settings for its own internet sessions.
fn playout_config() -> PlayoutConfig {
    PlayoutConfig::adaptive(60.0, AdaptiveConfig::WAN)
}

/// One audio stream from the phone, playing.
pub struct Playback {
    receiver: ReceiverRuntime,
    hello: HelloSender,
    stop: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pub device: String,
}

impl std::fmt::Debug for Playback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Playback")
            .field("device", &self.device)
            .finish_non_exhaustive()
    }
}

impl Playback {
    /// Receives on `udp` with the media key `key` and plays to the default
    /// output. Sends the first UDP hello, so the relay starts forwarding.
    pub fn start(udp: Udp, key: [u8; 32]) -> Result<Playback, String> {
        let hello = udp.hello_sender()?;
        let (receiver, source) = ReceiverRuntime::start(
            udp.socket,
            &PresharedKey::from_bytes(key),
            ReceiverConfig::default(),
            playout_config(),
            None,
        )
        .map_err(|e| format!("Could not start receiving audio: {e}"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let failed = Arc::new(AtomicBool::new(false));
        let (thread, device) = spawn_output(source, Arc::clone(&stop), Arc::clone(&failed))?;
        hello.send();
        Ok(Playback {
            receiver,
            hello,
            stop,
            failed,
            thread: Some(thread),
            device,
        })
    }

    /// Repeats the UDP hello.
    pub fn keep_alive(&self) {
        self.hello.send();
    }

    /// Whether the output device failed (it went away or changed), so the
    /// stream must start again.
    pub fn failed(&self) -> bool {
        self.failed.load(Relaxed)
    }

    pub fn receiver_stats(&self) -> &Arc<ReceiverStats> {
        self.receiver.stats()
    }

    pub fn playout_stats(&self) -> &Arc<PlayoutStats> {
        self.receiver.playout_stats()
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(t) = self.thread.take() {
            t.thread().unpark();
            let _ = t.join();
        }
    }
}

/// Opens the default output on a thread that owns the stream until `stop`.
fn spawn_output(
    mut source: PlayoutSource,
    stop: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
) -> Result<(JoinHandle<()>, String), String> {
    let (tx, rx) = mpsc::sync_channel(1);
    let thread = std::thread::Builder::new()
        .name("remote-audio-out".into())
        .spawn(move || {
            let built = (|| {
                let host = cpal::default_host();
                let device = host
                    .default_output_device()
                    .ok_or_else(|| "This device has no audio output.".to_owned())?;
                let config = device
                    .default_output_config()
                    .map_err(|e| format!("Could not read the audio output's format: {e}"))?;
                let rate = config.sample_rate();
                let channels = usize::from(config.channels());
                // Control path: the playout allocates for this format here.
                source.prepare(rate, channels, rate as usize)?;
                let format = (rate, config.channels());
                let probe = device.clone();
                let on_error = {
                    let failed = Arc::clone(&failed);
                    move |e: cpal::Error| {
                        let current = || {
                            probe
                                .default_output_config()
                                .ok()
                                .map(|c| (c.sample_rate(), c.channels()))
                        };
                        if is_failure(&e, format, current) {
                            failed.store(true, Relaxed);
                        }
                    }
                };
                let stream = device
                    .build_output_stream::<f32, _, _>(
                        config.config(),
                        move |data: &mut [f32], _| source.render(data, clock::now_ns()),
                        on_error,
                        None,
                    )
                    .map_err(|e| format!("Could not open the audio output: {e}"))?;
                stream
                    .play()
                    .map_err(|e| format!("Could not start the audio output: {e}"))?;
                let name = device
                    .description()
                    .map(|d| d.name().to_owned())
                    .unwrap_or_else(|_| "Audio output".into());
                Ok::<_, String>((stream, name))
            })();
            match built {
                Ok((stream, name)) => {
                    let _ = tx.send(Ok(name));
                    while !stop.load(Relaxed) {
                        std::thread::park_timeout(Duration::from_millis(500));
                    }
                    drop(stream);
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                }
            }
        })
        .map_err(|e| format!("Could not start the audio thread: {e}"))?;
    match rx.recv() {
        Ok(Ok(name)) => Ok((thread, name)),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => Err("The audio thread stopped unexpectedly.".into()),
    }
}

/// Whether a cpal stream error means the stream must start again, as
/// AudioNet decides for its own streams: an underrun or overrun (AAudio
/// reports each one) is a glitch the platform already recovered from, and
/// so is denied real-time priority. On iOS a running stream follows a new
/// route (headphones, a call) by itself; only a new sample rate or
/// channel count needs a new stream. This can run in the audio callback,
/// so for those cases it only reads the error.
fn is_failure(
    e: &cpal::Error,
    format: (u32, u16),
    current: impl Fn() -> Option<(u32, u16)>,
) -> bool {
    match e.kind() {
        cpal::ErrorKind::Xrun | cpal::ErrorKind::RealtimeDenied => return false,
        _ => {}
    }
    if cfg!(target_os = "ios") {
        match e.kind() {
            cpal::ErrorKind::DeviceChanged => return false,
            cpal::ErrorKind::StreamInvalidated => return current() != Some(format),
            _ => {}
        }
    }
    true
}

/// The stream's state in words, one "Label: value" per line, for the
/// controller's connection details.
pub fn describe(receiver: &ReceiverStats, playout: &PlayoutStats) -> String {
    let r = receiver.snapshot();
    let p = playout.snapshot();
    let mut lines = Vec::new();
    lines.push(format!(
        "Audio: {}",
        if p.playing {
            "playing"
        } else {
            "waiting for audio"
        }
    ));
    lines.push(format!("Buffer target: {:.0} milliseconds", p.target_ms));
    lines.push(format!("Packets received: {}", r.datagrams));
    lines.push(format!("Packets lost: {:.1} percent", r.loss_percent));
    lines.push(format!("Network jitter: {:.1} milliseconds", r.jitter_ms));
    lines.push(format!("Late packets: {}", r.late_packets));
    lines.push(format!("Concealed frames: {}", r.concealed_frames));
    lines.push(format!("Playback underruns: {}", p.underruns));
    lines.push(format!(
        "Packets that failed decryption: {}",
        r.auth_failures
    ));
    lines.join("\n")
}
