//! Host and controller through a real relay on this machine.

use std::net::SocketAddr;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use audionet_audio::clock;
use audionet_audio::render::RenderSource;
use audionet_engine::receiver::ReceiverConfig;
use audionet_engine::runtime::ReceiverRuntime;
use audionet_transport::crypto::PresharedKey;
use backtalk_remote_protocol::Role;

use crate::controller::{ControllerListener, ControllerSession, ControllerState, ControllerStatus};
use crate::host::{HostListener, HostSession, HostState, HostStatus};
use crate::host_audio::HostAudio;
use crate::keys::{self, Keys};
use crate::link::{self, Server, Udp};
use crate::peer::{GlobalAction, Screen, TouchAction, TouchEvent, TouchPointer};

/// Starts a relay on this machine; returns its server setting.
fn relay() -> (String, tokio::runtime::Runtime) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let url = rt.block_on(async {
        let udp = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = udp.local_addr().unwrap().port();
        let relay = backtalk_remote_relay::Relay::new(udp, port, Default::default());
        tokio::spawn(relay.clone().run_udp());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = relay
            .router()
            .into_make_service_with_connect_info::<SocketAddr>();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("ws://{addr}")
    });
    (url, rt)
}

#[derive(Debug)]
enum HostEvent {
    Status(HostStatus),
    Touch(TouchEvent),
    Global(GlobalAction),
    Text(String),
    Audio(bool),
}

struct HostProbe(Mutex<Sender<HostEvent>>);

impl HostListener for HostProbe {
    fn on_status(&self, status: HostStatus) {
        let _ = self.0.lock().unwrap().send(HostEvent::Status(status));
    }
    fn on_touch(&self, _controller: u32, event: TouchEvent) {
        let _ = self.0.lock().unwrap().send(HostEvent::Touch(event));
    }
    fn on_global_action(&self, action: GlobalAction) {
        let _ = self.0.lock().unwrap().send(HostEvent::Global(action));
    }
    fn on_type_text(&self, text: String) {
        let _ = self.0.lock().unwrap().send(HostEvent::Text(text));
    }
    fn on_audio_wanted(&self, wanted: bool) {
        let _ = self.0.lock().unwrap().send(HostEvent::Audio(wanted));
    }
}

struct ControllerProbe(Mutex<Sender<ControllerStatus>>);

impl ControllerListener for ControllerProbe {
    fn on_status(&self, status: ControllerStatus) {
        let _ = self.0.lock().unwrap().send(status);
    }
}

fn wait_for<T: std::fmt::Debug>(
    rx: &Receiver<T>,
    what: &str,
    mut want: impl FnMut(&T) -> bool,
) -> T {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(v) if want(&v) => return v,
            Ok(_) => {}
            Err(_) => panic!("timed out waiting for {what}"),
        }
    }
}

const SCREEN: Screen = Screen {
    width: 1080,
    height: 2400,
};

#[test]
fn a_controller_controls_the_phone_with_the_same_key() {
    let (server, _rt) = relay();
    let (htx, hrx) = channel();
    let host = HostSession::start(
        server.clone(),
        "test access key 1".into(),
        "Test Pixel".into(),
        SCREEN,
        Arc::new(HostProbe(Mutex::new(htx))),
    );
    wait_for(
        &hrx,
        "the phone online",
        |e| matches!(e, HostEvent::Status(s) if s.state == HostState::Waiting),
    );

    let (ctx, crx) = channel();
    let controller = ControllerSession::start(
        server.clone(),
        " test access key 1 ".into(),
        "Test iPhone".into(),
        Arc::new(ControllerProbe(Mutex::new(ctx))),
    );
    let status = wait_for(&crx, "the controller connected", |s| {
        s.state == ControllerState::Connected
    });
    assert_eq!(status.phone.as_deref(), Some("Test Pixel"));
    assert_eq!(status.screen, Some(SCREEN));
    // Backtalk is asked for its audio before the status says controlled.
    wait_for(&hrx, "audio wanted", |e| {
        matches!(e, HostEvent::Audio(true))
    });
    let status = wait_for(
        &hrx,
        "the phone controlled",
        |e| matches!(e, HostEvent::Status(s) if s.state == HostState::Controlled),
    );
    let HostEvent::Status(status) = status else {
        unreachable!()
    };
    assert_eq!(status.controllers, vec!["Test iPhone".to_owned()]);

    let touch = TouchEvent {
        action: TouchAction::Down,
        changed: 0,
        time_ms: 1234,
        pointers: vec![TouchPointer {
            id: 0,
            x: 0.25,
            y: 0.75,
        }],
    };
    controller.send_touch(touch.clone());
    let lifted = TouchEvent {
        action: TouchAction::Up,
        time_ms: 1300,
        ..touch.clone()
    };
    controller.send_touch(lifted.clone());
    controller.send_global_action(GlobalAction::Home);
    controller.send_text("héllo".into());
    let mut got = Vec::new();
    while got.len() < 4 {
        match hrx
            .recv_timeout(Duration::from_secs(10))
            .expect("controller input")
        {
            HostEvent::Touch(e) => got.push(format!("{:?} {}", e.action, e.time_ms)),
            HostEvent::Global(a) => got.push(format!("{a:?}")),
            HostEvent::Text(t) => got.push(t),
            _ => {}
        }
    }
    // In the order sent.
    assert_eq!(got, vec!["Down 1234", "Up 1300", "Home", "héllo"]);

    controller.stop();
    wait_for(&crx, "the controller stopped", |s| {
        s.state == ControllerState::Stopped
    });
    wait_for(
        &hrx,
        "the phone waiting again",
        |e| matches!(e, HostEvent::Status(s) if s.state == HostState::Waiting && s.controllers.is_empty()),
    );

    // A second controller, then the phone stops sharing.
    let (ctx, crx) = channel();
    let controller = ControllerSession::start(
        server,
        "test access key 1".into(),
        "Second".into(),
        Arc::new(ControllerProbe(Mutex::new(ctx))),
    );
    wait_for(&crx, "the second controller connected", |s| {
        s.state == ControllerState::Connected
    });
    host.stop();
    let status = wait_for(&crx, "the phone's goodbye", |s| {
        s.state == ControllerState::WaitingForPhone
    });
    assert_eq!(status.detail.as_deref(), Some("The phone stopped sharing."));
    wait_for(
        &hrx,
        "the phone stopped",
        |e| matches!(e, HostEvent::Status(s) if s.state == HostState::Stopped),
    );
    controller.stop();
}

#[test]
fn a_wrong_key_finds_no_phone() {
    let (server, _rt) = relay();
    let (htx, hrx) = channel();
    let host = HostSession::start(
        server.clone(),
        "the right key".into(),
        "Phone".into(),
        SCREEN,
        Arc::new(HostProbe(Mutex::new(htx))),
    );
    wait_for(
        &hrx,
        "the phone online",
        |e| matches!(e, HostEvent::Status(s) if s.state == HostState::Waiting),
    );
    let (ctx, crx) = channel();
    let controller = ControllerSession::start(
        server,
        "the wrong key".into(),
        "Controller".into(),
        Arc::new(ControllerProbe(Mutex::new(ctx))),
    );
    wait_for(&crx, "waiting for a phone", |s| {
        s.state == ControllerState::WaitingForPhone
    });
    // And it stays that way.
    let later = crx.recv_timeout(Duration::from_secs(2));
    assert!(
        later.as_ref().is_err() || later.as_ref().unwrap().state != ControllerState::Connected,
        "{later:?}"
    );
    controller.stop();
    host.stop();
}

#[test]
fn bad_settings_fail_with_a_reason() {
    let (ctx, crx) = channel();
    let _c = ControllerSession::start(
        "".into(),
        "long enough key".into(),
        "C".into(),
        Arc::new(ControllerProbe(Mutex::new(ctx))),
    );
    let s = wait_for(&crx, "failure", |s| s.state == ControllerState::Failed);
    assert_eq!(s.detail.as_deref(), Some("Enter a relay server."));
    let (ctx, crx) = channel();
    let _c = ControllerSession::start(
        "example.org".into(),
        "short".into(),
        "C".into(),
        Arc::new(ControllerProbe(Mutex::new(ctx))),
    );
    let s = wait_for(&crx, "failure", |s| s.state == ControllerState::Failed);
    assert_eq!(
        s.detail.as_deref(),
        Some("The access key needs at least 8 characters.")
    );
}

/// Backtalk's audio from the phone, through the relay's UDP path, with
/// AudioNet's sender and receiver, including the silence the pacer adds
/// between sounds.
#[test]
fn audio_goes_through_the_relay() {
    let (url, rt) = relay();
    let server = Server::parse(&url).unwrap();
    let keys = Keys::derive("audio test key").unwrap();
    let stream_nonce = keys::nonce();
    let media = keys.media(&stream_nonce);
    let (host_join, ctrl_join) = rt.block_on(async {
        let (h, hj) = link::join(&server, &keys.room, Role::Host).await.unwrap();
        let (c, cj) = link::join(&server, &keys.room, Role::Controller)
            .await
            .unwrap();
        // Keep the WebSockets open for the test.
        std::mem::forget(h);
        std::mem::forget(c);
        (hj, cj)
    });
    let host_udp = Udp::open(&server, &host_join).unwrap();
    let ctrl_udp = Udp::open(&server, &ctrl_join).unwrap();
    host_udp.hello_sender().unwrap().send();
    let ctrl_hello = ctrl_udp.hello_sender().unwrap();
    ctrl_hello.send();
    std::thread::sleep(Duration::from_millis(200));

    let (receiver, mut source) = ReceiverRuntime::start(
        ctrl_udp.socket,
        &PresharedKey::from_bytes(media),
        ReceiverConfig::default(),
        audionet_engine::playout::PlayoutConfig::default(),
        None,
    )
    .unwrap();
    source.prepare(48_000, 2, 4800).unwrap();
    let audio =
        HostAudio::start(host_udp.socket.try_clone().unwrap(), host_udp.relay, media).unwrap();

    let start = Instant::now();
    let mut out = vec![0.0f32; 960];
    let mut peak_tone = 0.0f32;
    let mut peak_silence = 0.0f32;
    // 1 second of a 997 Hz tone in 10 ms blocks, as the app hands over
    // Backtalk's mixer output, then a second of nothing: the pacer's
    // silence.
    for tick in 0..200u64 {
        if tick < 100 {
            let mut bytes = Vec::with_capacity(480 * 8);
            for i in 0..480u64 {
                let t = (tick * 480 + i) as f32 / 48_000.0;
                let v = 0.25 * (2.0 * std::f32::consts::PI * 997.0 * t).sin();
                bytes.extend_from_slice(&v.to_le_bytes());
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            audio.push(48_000, &bytes).unwrap();
        }
        let due = start + Duration::from_millis(tick * 10);
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        source.render(&mut out, clock::now_ns());
        let peak = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        if (40..100).contains(&tick) {
            peak_tone = peak_tone.max(peak);
        } else if tick > 180 {
            peak_silence = peak_silence.max(peak);
        }
    }
    let rx = receiver.stats().snapshot();
    let playout = receiver.playout_stats().snapshot();
    assert_eq!(rx.auth_failures, 0);
    // Packets kept coming through the silence: 2 seconds of 10 ms frames.
    assert!(rx.datagrams >= 170, "received {}", rx.datagrams);
    assert!(playout.playing);
    assert!((0.2..0.3).contains(&peak_tone), "tone peak {peak_tone}");
    assert!(peak_silence < 0.01, "silence peak {peak_silence}");
    drop(audio);
    drop(ctrl_hello);
}
