//! `remote-probe`: a controlling device for testing, without a screen.
//!
//! Connects to the phone sharing with an access key, sends scripted
//! gestures, and records the phone's audio to a WAV file, printing what
//! happens as "Label: value" lines.
//!
//! ```text
//! remote-probe --server mad-gamer.com --key ACCESS-KEY [--wav out.wav] [--script STEPS]
//! ```
//!
//! STEPS are separated by commas: `wait:MS`, `right`, `left`, `up`, `down`
//! (one-finger swipes), `tap`, `doubletap`, `explore:X:Y` (touch and hold at
//! a point, 0 to 1 across and down), `twoleft`, `tworight` (two-finger
//! swipes), `twoup`, `twodown` (two-finger scrolls), `home`, `back`,
//! `type:TEXT`.

use std::io::Write;
use std::time::{Duration, Instant};

use audionet_audio::clock;
use audionet_audio::render::RenderSource;
use audionet_engine::receiver::ReceiverConfig;
use audionet_engine::runtime::ReceiverRuntime;
use audionet_transport::crypto::PresharedKey;
use backtalk_remote::keys::{self, Keys, NONCE_LEN};
use backtalk_remote::link::{self, Incoming, Server, Socket, Udp};
use backtalk_remote::peer::{
    GlobalAction, PEER_VERSION, Sequence, ToController, ToHost, TouchAction, TouchEvent,
    TouchPointer,
};
use backtalk_remote::seal::{Direction, Kind, Sealer};
use backtalk_remote_protocol::{RelayEvent, RelayRequest, Role, from_hex, to_hex};

fn usage() -> ! {
    eprintln!("Usage: remote-probe --server SERVER --key KEY [--wav FILE] [--script STEPS]");
    std::process::exit(2);
}

struct Session {
    host: u32,
    sealer: Sealer,
    sequence: Sequence,
    start: Instant,
}

impl Session {
    async fn send(&mut self, socket: &mut Socket, body: impl FnOnce(u64) -> ToHost) {
        let message = body(self.sequence.next_out());
        let data = self
            .sealer
            .seal(Direction::ToHost, &serde_json::to_vec(&message).unwrap());
        link::send(
            socket,
            &RelayRequest::Send {
                to: self.host,
                data,
            },
        )
        .await
        .expect("send");
    }

    fn now(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    async fn touch(
        &mut self,
        socket: &mut Socket,
        action: TouchAction,
        changed: u32,
        points: &[(u32, f32, f32)],
    ) {
        let event = TouchEvent {
            action,
            changed,
            time_ms: self.now(),
            pointers: points
                .iter()
                .map(|&(id, x, y)| TouchPointer { id, x, y })
                .collect(),
        };
        self.send(socket, |seq| ToHost::Touch { seq, event }).await;
    }

    /// One finger from `from` to `to` in `ms`, in 10 ms steps.
    async fn stroke(&mut self, socket: &mut Socket, from: (f32, f32), to: (f32, f32), ms: u64) {
        self.touch(socket, TouchAction::Down, 0, &[(0, from.0, from.1)])
            .await;
        let steps = (ms / 10).max(1);
        for i in 1..=steps {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let t = i as f32 / steps as f32;
            let p = (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
            self.touch(socket, TouchAction::Move, 0, &[(0, p.0, p.1)])
                .await;
        }
        self.touch(socket, TouchAction::Up, 0, &[(0, to.0, to.1)])
            .await;
    }

    /// Two fingers side by side, moving by (`dx`, `dy`) over `steps` 10 ms steps.
    async fn two_finger(&mut self, socket: &mut Socket, dx: f32, dy: f32, steps: u32) {
        let (a, b) = (
            (0.45 - dx / 2.0, 0.5 - dy / 2.0),
            (0.55 - dx / 2.0, 0.5 - dy / 2.0),
        );
        self.touch(socket, TouchAction::Down, 0, &[(0, a.0, a.1)])
            .await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        self.touch(
            socket,
            TouchAction::PointerDown,
            1,
            &[(0, a.0, a.1), (1, b.0, b.1)],
        )
        .await;
        for i in 1..=steps {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let (x, y) = (dx * i as f32 / steps as f32, dy * i as f32 / steps as f32);
            self.touch(
                socket,
                TouchAction::Move,
                0,
                &[(0, a.0 + x, a.1 + y), (1, b.0 + x, b.1 + y)],
            )
            .await;
        }
        let (x, y) = (dx, dy);
        self.touch(
            socket,
            TouchAction::PointerUp,
            1,
            &[(0, a.0 + x, a.1 + y), (1, b.0 + x, b.1 + y)],
        )
        .await;
        self.touch(socket, TouchAction::Up, 0, &[(0, a.0 + x, a.1 + y)])
            .await;
    }

    async fn tap(&mut self, socket: &mut Socket, x: f32, y: f32) {
        self.touch(socket, TouchAction::Down, 0, &[(0, x, y)]).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        self.touch(socket, TouchAction::Up, 0, &[(0, x, y)]).await;
    }

    async fn step(&mut self, socket: &mut Socket, step: &str) {
        let parts: Vec<&str> = step.split(':').collect();
        println!("Step: {step}");
        match parts.as_slice() {
            ["wait", ms] => tokio::time::sleep(Duration::from_millis(ms.parse().unwrap())).await,
            ["right"] => self.stroke(socket, (0.3, 0.5), (0.7, 0.5), 120).await,
            ["left"] => self.stroke(socket, (0.7, 0.5), (0.3, 0.5), 120).await,
            ["up"] => self.stroke(socket, (0.5, 0.6), (0.5, 0.4), 120).await,
            ["down"] => self.stroke(socket, (0.5, 0.4), (0.5, 0.6), 120).await,
            ["tap"] => self.tap(socket, 0.5, 0.5).await,
            ["doubletap"] => {
                self.tap(socket, 0.5, 0.5).await;
                tokio::time::sleep(Duration::from_millis(100)).await;
                self.tap(socket, 0.5, 0.5).await;
            }
            ["explore", x, y] => {
                let (x, y) = (x.parse().unwrap(), y.parse().unwrap());
                self.touch(socket, TouchAction::Down, 0, &[(0, x, y)]).await;
                for _ in 0..60 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    self.touch(socket, TouchAction::Move, 0, &[(0, x, y)]).await;
                }
                self.touch(socket, TouchAction::Up, 0, &[(0, x, y)]).await;
            }
            ["twoleft"] => self.two_finger(socket, -0.4, 0.0, 15).await,
            ["tworight"] => self.two_finger(socket, 0.4, 0.0, 15).await,
            // A slow drag, so it scrolls rather than being a swipe gesture.
            ["twoup"] => self.two_finger(socket, 0.0, -0.4, 60).await,
            ["twodown"] => self.two_finger(socket, 0.0, 0.4, 60).await,
            ["home"] => {
                self.send(socket, |seq| ToHost::Global {
                    seq,
                    action: GlobalAction::Home,
                })
                .await
            }
            ["back"] => {
                self.send(socket, |seq| ToHost::Global {
                    seq,
                    action: GlobalAction::Back,
                })
                .await
            }
            ["type", text] => {
                let text = text.to_string();
                self.send(socket, |seq| ToHost::Type { seq, text }).await
            }
            _ => eprintln!("Unknown step: {step}"),
        }
    }
}

/// Records 48 kHz stereo to a 16-bit WAV file.
fn write_wav(path: &str, samples: &[f32]) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    let data_len = (samples.len() * 2) as u32;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&48_000u32.to_le_bytes())?;
    f.write_all(&(48_000u32 * 4).to_le_bytes())?;
    f.write_all(&4u16.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    for s in samples {
        f.write_all(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())?;
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    let mut server = None;
    let mut key = None;
    let mut wav = None;
    let mut script = String::from("wait:2000,right,wait:1500,right,wait:1500,left,wait:1500");
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let v = args.next().unwrap_or_else(|| usage());
        match a.as_str() {
            "--server" => server = Some(v),
            "--key" => key = Some(v),
            "--wav" => wav = Some(v),
            "--script" => script = v,
            _ => usage(),
        }
    }
    let server = Server::parse(&server.unwrap_or_else(|| usage())).expect("server");
    let keys = Keys::derive(&key.unwrap_or_else(|| usage())).expect("key");
    let (mut socket, joined) = link::join(&server, &keys.room, Role::Controller)
        .await
        .expect("join");
    println!("Relay: joined as peer {}", joined.peer_id);
    let Some(host) = joined
        .peers
        .iter()
        .find(|p| p.role == Role::Host)
        .map(|p| p.peer_id)
    else {
        println!("Phone: none is sharing with this key");
        std::process::exit(1);
    };
    let handshake = Sealer::new(&keys.handshake(), Kind::Handshake);
    let nonce = keys::nonce();
    let hello = ToHost::Hello {
        version: PEER_VERSION,
        nonce: to_hex(&nonce),
        name: "remote-probe".into(),
    };
    let data = handshake.seal(Direction::ToHost, &serde_json::to_vec(&hello).unwrap());
    link::send(&mut socket, &RelayRequest::Send { to: host, data })
        .await
        .unwrap();
    let (session_key, stream_nonce) = loop {
        match link::next(&mut socket).await.unwrap() {
            Some(Incoming::Event(RelayEvent::Message { from, data })) if from == host => {
                let plain = handshake
                    .open(Direction::ToController, &data)
                    .expect("welcome opens");
                let ToController::Welcome {
                    controller_nonce,
                    host_nonce,
                    stream_nonce,
                    name,
                    screen,
                    ..
                } = serde_json::from_slice(&plain).unwrap()
                else {
                    continue;
                };
                assert_eq!(from_hex::<NONCE_LEN>(&controller_nonce), Some(nonce));
                println!(
                    "Phone: {name}, screen {} by {}",
                    screen.width, screen.height
                );
                let host_nonce = from_hex::<NONCE_LEN>(&host_nonce).unwrap();
                break (
                    keys.session(&nonce, &host_nonce),
                    from_hex::<NONCE_LEN>(&stream_nonce).unwrap(),
                );
            }
            Some(_) => {}
            None => panic!("relay closed"),
        }
    };
    let mut session = Session {
        host,
        sealer: Sealer::new(&session_key, Kind::Session),
        sequence: Sequence::default(),
        start: Instant::now(),
    };

    let udp = Udp::open(&server, &joined).expect("udp");
    let hello = udp.hello_sender().unwrap();
    let (receiver, mut source) = ReceiverRuntime::start(
        udp.socket,
        &PresharedKey::from_bytes(keys.media(&stream_nonce)),
        ReceiverConfig::default(),
        audionet_engine::playout::PlayoutConfig::adaptive(
            60.0,
            audionet_engine::adaptive::AdaptiveConfig::WAN,
        ),
        None,
    )
    .unwrap();
    hello.send();
    source.prepare(48_000, 2, 4800).unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let recorder = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut all = Vec::new();
            let mut block = vec![0.0f32; 960];
            let start = Instant::now();
            let mut tick = 0u64;
            let mut second_peak = 0.0f32;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                tick += 1;
                let due = start + Duration::from_millis(tick * 10);
                if let Some(w) = due.checked_duration_since(Instant::now()) {
                    std::thread::sleep(w);
                }
                source.render(&mut block, clock::now_ns());
                second_peak = block.iter().fold(second_peak, |m, v| m.max(v.abs()));
                all.extend_from_slice(&block);
                if tick % 100 == 0 {
                    println!("Audio level, second {}: {:.3}", tick / 100, second_peak);
                    second_peak = 0.0;
                }
            }
            all
        })
    };
    let hellos = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            hello.send();
        }
    });

    for step in script.split(',').filter(|s| !s.is_empty()) {
        session.step(&mut socket, step).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let bye = |seq| ToHost::Bye { seq };
    session.send(&mut socket, bye).await;
    hellos.abort();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let samples = recorder.join().unwrap();
    let r = receiver.stats().snapshot();
    let p = receiver.playout_stats().snapshot();
    println!("Packets received: {}", r.datagrams);
    println!("Packets lost: {:.1} percent", r.loss_percent);
    println!("Packets that failed decryption: {}", r.auth_failures);
    println!("Network jitter: {:.1} milliseconds", r.jitter_ms);
    println!("Buffer target: {:.0} milliseconds", p.target_ms);
    println!("Playback underruns: {}", p.underruns);
    println!("Late packets: {}", r.late_packets);
    println!("Concealed frames: {}", r.concealed_frames);
    println!("Stream starts: {}", r.stream_starts);
    println!("Outage resyncs: {}", r.outage_resyncs);
    println!("Primings: {}", p.primings);
    println!("Silence frames: {}", p.silence_frames);
    println!(
        "Latency drains: {} ({} frames)",
        p.latency_drains, p.latency_drain_frames
    );
    println!("Catastrophic trims: {}", p.catastrophic_trims);
    println!("Interarrival: {:?}", r.interarrival_ns);
    println!("Depth: {:?}", p.depth_us);
    if let Some(path) = wav {
        write_wav(&path, &samples).unwrap();
        println!("Recording: {path}");
    }
}
