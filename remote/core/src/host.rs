//! The phone's side: share this phone with the devices that have its
//! access key.
//!
//! One task holds the relay connection. It answers each controller's
//! hello, opens what they send and passes touches, actions and text to the
//! app; it starts the audio stream when the first controller arrives and
//! stops it when the last leaves. When the connection drops, it connects
//! again, waiting longer each time, until the app stops it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use backtalk_remote_protocol::{
    ErrorCode, RelayEvent, RelayRequest, Role, UDP_HELLO_INTERVAL_MS, from_hex, to_hex,
};
use tokio::sync::mpsc;

use crate::host_audio::HostAudio;
use crate::keys::{self, Keys, NONCE_LEN};
use crate::link::{self, Incoming, Server, Udp};
use crate::peer::{
    self, GlobalAction, MAX_TYPED_CHARS, PEER_VERSION, Screen, Sequence, ToController, ToHost,
    TouchEvent,
};
use crate::seal::{self, Direction, Kind, Sealer};
use crate::{Backoff, RELAY_SILENCE, runtime};

/// What the phone is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum HostState {
    /// Connecting to the relay for the first time.
    Connecting,
    /// Connected, and no one is controlling the phone.
    Waiting,
    /// One or more devices are controlling the phone.
    Controlled,
    /// The connection dropped; connecting again.
    Reconnecting,
    /// Stopped by the app.
    Stopped,
    /// Stopped by a problem that trying again will not fix; `detail` says
    /// which.
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct HostStatus {
    pub state: HostState,
    /// The devices controlling the phone, by name.
    pub controllers: Vec<String>,
    /// What went wrong, in words, or `None`.
    pub detail: Option<String>,
}

/// The app's side of a host session. Called on the session's threads, not
/// the app's main thread.
#[uniffi::export(with_foreign)]
pub trait HostListener: Send + Sync {
    fn on_status(&self, status: HostStatus);
    /// A touch from `controller`, one of the ids in no particular order
    /// that stays the same while that device is connected.
    fn on_touch(&self, controller: u32, event: TouchEvent);
    fn on_global_action(&self, action: GlobalAction);
    /// Text to type into the focused field, at the cursor.
    fn on_type_text(&self, text: String);
    /// Whether to hand Backtalk's audio to [`HostSession::push_audio`]:
    /// true while a device is controlling the phone.
    fn on_audio_wanted(&self, wanted: bool);
}

enum Command {
    Screen(Screen),
    Stop,
}

/// Shares this phone through the relay until [`stop`](Self::stop).
#[derive(uniffi::Object)]
pub struct HostSession {
    commands: mpsc::UnboundedSender<Command>,
    audio: Arc<Mutex<Option<HostAudio>>>,
}

impl std::fmt::Debug for HostSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostSession").finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[uniffi::export]
impl HostSession {
    /// Starts sharing. `server` is the relay server setting; `name` names
    /// this phone for its controllers; `screen` is its size now. Problems
    /// come to `listener` as statuses, never as errors here.
    #[uniffi::constructor]
    pub fn start(
        server: String,
        access_key: String,
        name: String,
        screen: Screen,
        listener: Arc<dyn HostListener>,
    ) -> Arc<Self> {
        let (commands, receiver) = mpsc::unbounded_channel();
        let audio = Arc::new(Mutex::new(None));
        let task = Task {
            listener,
            name: peer::clean_name(&name),
            screen,
            audio: Arc::clone(&audio),
            last_status: None,
        };
        runtime().spawn(task.run(server, access_key, receiver));
        Arc::new(Self { commands, audio })
    }

    /// Backtalk's audio as it plays: interleaved stereo at `sample_rate`,
    /// 32-bit floats, little-endian. Dropped while no device is
    /// controlling the phone. Never waits on the network.
    pub fn push_audio(&self, sample_rate: u32, samples: Vec<u8>) {
        if let Some(audio) = lock(&self.audio).as_ref() {
            let _ = audio.push(sample_rate, &samples);
        }
    }

    /// The screen turned or changed size.
    pub fn set_screen(&self, screen: Screen) {
        let _ = self.commands.send(Command::Screen(screen));
    }

    /// Stops sharing and tells the controllers.
    pub fn stop(&self) {
        let _ = self.commands.send(Command::Stop);
    }
}

struct Controller {
    name: String,
    sealer: Sealer,
    sequence: Sequence,
}

enum Ended {
    Stopped,
    /// Try again: the connection dropped or the relay could not be
    /// reached.
    Lost(String),
    /// Do not try again.
    Fatal(String),
}

struct Task {
    listener: Arc<dyn HostListener>,
    name: String,
    screen: Screen,
    audio: Arc<Mutex<Option<HostAudio>>>,
    last_status: Option<HostStatus>,
}

impl Task {
    fn status(
        &mut self,
        state: HostState,
        controllers: &HashMap<u32, Controller>,
        detail: Option<String>,
    ) {
        let mut names: Vec<String> = controllers.values().map(|c| c.name.clone()).collect();
        names.sort();
        let status = HostStatus {
            state,
            controllers: names,
            detail,
        };
        if self.last_status.as_ref() != Some(&status) {
            self.last_status = Some(status.clone());
            self.listener.on_status(status);
        }
    }

    fn set_audio(&mut self, audio: Option<HostAudio>) {
        let wanted = audio.is_some();
        let had = {
            let mut slot = lock(&self.audio);
            let had = slot.is_some();
            *slot = audio;
            had
        };
        if had != wanted {
            self.listener.on_audio_wanted(wanted);
        }
    }

    async fn run(
        mut self,
        server: String,
        access_key: String,
        mut commands: mpsc::UnboundedReceiver<Command>,
    ) {
        let none = HashMap::new();
        self.status(HostState::Connecting, &none, None);
        let server = match Server::parse(&server) {
            Ok(s) => s,
            Err(e) => return self.status(HostState::Failed, &none, Some(e)),
        };
        let keys = match tokio::task::spawn_blocking(move || Keys::derive(&access_key)).await {
            Ok(Ok(k)) => k,
            Ok(Err(problem)) => {
                return self.status(HostState::Failed, &none, Some(problem.to_string()));
            }
            Err(_) => {
                return self.status(
                    HostState::Failed,
                    &none,
                    Some("Could not prepare the access key.".into()),
                );
            }
        };
        let mut backoff = Backoff::default();
        loop {
            match self
                .serve(&server, &keys, &mut commands, &mut backoff)
                .await
            {
                Ended::Stopped => {
                    self.set_audio(None);
                    return self.status(HostState::Stopped, &none, None);
                }
                Ended::Fatal(why) => {
                    self.set_audio(None);
                    return self.status(HostState::Failed, &none, Some(why));
                }
                Ended::Lost(why) => {
                    self.set_audio(None);
                    self.status(HostState::Reconnecting, &none, Some(why));
                }
            }
            let wait = backoff.delay();
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(wait) => break,
                    command = commands.recv() => match command {
                        Some(Command::Screen(s)) => self.screen = s,
                        Some(Command::Stop) | None => {
                            return self.status(HostState::Stopped, &none, None);
                        }
                    },
                }
            }
        }
    }

    async fn serve(
        &mut self,
        server: &Server,
        keys: &Keys,
        commands: &mut mpsc::UnboundedReceiver<Command>,
        backoff: &mut Backoff,
    ) -> Ended {
        let (mut socket, joined) = match link::join(server, &keys.room, Role::Host).await {
            Ok(j) => j,
            Err(e) => return Ended::Lost(e),
        };
        let open_server = server.clone();
        let open_joined = joined.clone();
        let udp = match tokio::task::spawn_blocking(move || Udp::open(&open_server, &open_joined))
            .await
        {
            Ok(Ok(u)) => u,
            Ok(Err(e)) => return Ended::Lost(e),
            Err(_) => return Ended::Lost("Could not open the audio socket.".into()),
        };
        let hello = match udp.hello_sender() {
            Ok(h) => h,
            Err(e) => return Ended::Lost(e),
        };
        hello.send();
        let handshake = Sealer::new(&keys.handshake(), Kind::Handshake);
        let stream_nonce = keys::nonce();
        let media_key = keys.media(&stream_nonce);
        let mut controllers: HashMap<u32, Controller> = HashMap::new();
        self.status(HostState::Waiting, &controllers, None);
        let mut hello_timer = tokio::time::interval(Duration::from_millis(UDP_HELLO_INTERVAL_MS));
        let mut heard = Instant::now();
        let mut connected_since = Some(Instant::now());
        let mut audio_problem: Option<String> = None;
        loop {
            // A connection that stayed up a while resets the wait before
            // the next attempt.
            if connected_since.is_some_and(|t| t.elapsed() > Duration::from_secs(30)) {
                backoff.reset();
                connected_since = None;
            }
            tokio::select! {
                incoming = link::next(&mut socket) => {
                    let event = match incoming {
                        Ok(Some(Incoming::Event(e))) => e,
                        Ok(Some(Incoming::Alive)) => {
                            heard = Instant::now();
                            continue;
                        }
                        Ok(None) => return Ended::Lost("The relay server closed the connection.".into()),
                        Err(e) => return Ended::Lost(e),
                    };
                    heard = Instant::now();
                    match event {
                        RelayEvent::Message { from, data } => {
                            if let Some(reply) = self.on_message(keys, &handshake, &stream_nonce, &mut controllers, from, &data) {
                                if let Err(e) = link::send(&mut socket, &RelayRequest::Send { to: from, data: reply }).await {
                                    return Ended::Lost(e);
                                }
                            }
                            self.audio_for(&controllers, &udp, media_key, &mut audio_problem);
                        }
                        RelayEvent::PeerLeft { peer_id } => {
                            if controllers.remove(&peer_id).is_some() {
                                self.audio_for(&controllers, &udp, media_key, &mut audio_problem);
                            }
                        }
                        RelayEvent::Error { code, message, fatal } => {
                            if code == ErrorCode::Replaced || code == ErrorCode::Version {
                                return Ended::Fatal(message);
                            }
                            if fatal {
                                return Ended::Lost(message);
                            }
                        }
                        // Controllers say hello once they know the phone
                        // is here; UDP is known to work once audio flows.
                        RelayEvent::PeerJoined { .. } | RelayEvent::UdpReady | RelayEvent::Joined { .. } => {}
                    }
                }
                command = commands.recv() => match command {
                    Some(Command::Screen(screen)) => {
                        self.screen = screen;
                        for (id, c) in &mut controllers {
                            let message = ToController::Screen { seq: c.sequence.next_out(), screen };
                            let data = c.sealer.seal(Direction::ToController, &serde_json::to_vec(&message).expect("serializes"));
                            if let Err(e) = link::send(&mut socket, &RelayRequest::Send { to: *id, data }).await {
                                return Ended::Lost(e);
                            }
                        }
                    }
                    Some(Command::Stop) | None => {
                        for (id, c) in &mut controllers {
                            let message = ToController::Bye {
                                seq: c.sequence.next_out(),
                                reason: "The phone stopped sharing.".into(),
                            };
                            let data = c.sealer.seal(Direction::ToController, &serde_json::to_vec(&message).expect("serializes"));
                            let _ = link::send(&mut socket, &RelayRequest::Send { to: *id, data }).await;
                        }
                        let _ = socket.close(None).await;
                        return Ended::Stopped;
                    }
                },
                _ = hello_timer.tick() => {
                    hello.send();
                    if heard.elapsed() > RELAY_SILENCE {
                        return Ended::Lost("The relay server stopped answering.".into());
                    }
                }
            }
        }
    }

    /// Starts the audio stream when the first controller arrives, stops it
    /// when the last leaves, and reports who is connected.
    fn audio_for(
        &mut self,
        controllers: &HashMap<u32, Controller>,
        udp: &Udp,
        media_key: [u8; 32],
        problem: &mut Option<String>,
    ) {
        if controllers.is_empty() {
            self.set_audio(None);
            *problem = None;
            return self.status(HostState::Waiting, controllers, None);
        }
        if lock(&self.audio).is_none() && problem.is_none() {
            let started = udp
                .socket
                .try_clone()
                .map_err(|e| e.to_string())
                .and_then(|s| HostAudio::start(s, udp.relay, media_key));
            match started {
                Ok(audio) => self.set_audio(Some(audio)),
                Err(e) => *problem = Some(format!("Audio cannot be sent: {e}")),
            }
        }
        self.status(HostState::Controlled, controllers, problem.clone());
    }

    /// Opens a controller's message and acts on it. Returns the reply to
    /// send, if any (the welcome).
    fn on_message(
        &mut self,
        keys: &Keys,
        handshake: &Sealer,
        stream_nonce: &[u8; NONCE_LEN],
        controllers: &mut HashMap<u32, Controller>,
        from: u32,
        data: &str,
    ) -> Option<String> {
        match seal::kind_of(data)? {
            Kind::Handshake => {
                let plain = handshake.open(Direction::ToHost, data)?;
                let ToHost::Hello {
                    version,
                    nonce,
                    name,
                } = serde_json::from_slice(&plain).ok()?
                else {
                    return None;
                };
                if version != PEER_VERSION {
                    return None;
                }
                let controller_nonce = from_hex::<NONCE_LEN>(&nonce)?;
                let host_nonce = keys::nonce();
                let sealer =
                    Sealer::new(&keys.session(&controller_nonce, &host_nonce), Kind::Session);
                controllers.insert(
                    from,
                    Controller {
                        name: peer::clean_name(&name),
                        sealer,
                        sequence: Sequence::default(),
                    },
                );
                let welcome = ToController::Welcome {
                    version: PEER_VERSION,
                    controller_nonce: nonce,
                    host_nonce: to_hex(&host_nonce),
                    stream_nonce: to_hex(stream_nonce),
                    name: self.name.clone(),
                    screen: self.screen,
                };
                Some(handshake.seal(
                    Direction::ToController,
                    &serde_json::to_vec(&welcome).expect("serializes"),
                ))
            }
            Kind::Session => {
                let controller = controllers.get_mut(&from)?;
                let plain = controller.sealer.open(Direction::ToHost, data)?;
                let message: ToHost = serde_json::from_slice(&plain).ok()?;
                if !controller.sequence.accept(message.seq()?) {
                    return None;
                }
                match message {
                    ToHost::Touch { event, .. } => {
                        if peer::touch_is_valid(&event) {
                            self.listener.on_touch(from, event);
                        }
                    }
                    ToHost::Global { action, .. } => self.listener.on_global_action(action),
                    ToHost::Type { text, .. } => {
                        if !text.is_empty() && text.chars().count() <= MAX_TYPED_CHARS {
                            self.listener.on_type_text(text);
                        }
                    }
                    ToHost::Bye { .. } => {
                        controllers.remove(&from);
                    }
                    ToHost::Hello { .. } => {}
                }
                None
            }
        }
    }
}
