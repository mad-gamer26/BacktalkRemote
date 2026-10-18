//! The controlling device's side: control the phone that shares the same
//! access key, and hear its Backtalk.
//!
//! One task holds the relay connection. It says hello whenever the phone
//! is in the room, opens the phone's welcome, plays the phone's audio, and
//! sends the touches, actions and text the app gives it. When the
//! connection drops, it connects again, waiting longer each time, until the
//! app stops it.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use backtalk_remote_protocol::{
    ErrorCode, RelayEvent, RelayRequest, Role, UDP_HELLO_INTERVAL_MS, from_hex, to_hex,
};
use tokio::sync::mpsc;

use crate::audio_out::{self, Playback};
use crate::keys::{self, Keys, NONCE_LEN};
use crate::link::{self, Incoming, Joined, Server, Socket, Udp};
use crate::peer::{
    self, GlobalAction, MAX_TYPED_CHARS, PEER_VERSION, Screen, Sequence, ToController, ToHost,
    TouchEvent,
};
use crate::seal::{self, Direction, Kind, Sealer};
use crate::{Backoff, RELAY_SILENCE, runtime};

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ControllerState {
    /// Connecting to the relay for the first time.
    Connecting,
    /// Connected to the relay; no phone shares this access key right now.
    WaitingForPhone,
    /// Controlling the phone.
    Connected,
    /// The connection dropped; connecting again.
    Reconnecting,
    /// Stopped by the app.
    Stopped,
    /// Stopped by a problem that trying again will not fix; `detail` says
    /// which.
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AudioState {
    /// Not connected to a phone.
    Off,
    /// Connected; no audio has arrived yet, or none for a while.
    Waiting,
    Playing,
    /// Audio cannot play; `detail` says why.
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ControllerStatus {
    pub state: ControllerState,
    /// The phone's name, while connected.
    pub phone: Option<String>,
    pub screen: Option<Screen>,
    pub audio: AudioState,
    /// What went wrong, in words, or `None`.
    pub detail: Option<String>,
}

/// The app's side of a controller session. Called on the session's
/// threads, not the app's main thread.
#[uniffi::export(with_foreign)]
pub trait ControllerListener: Send + Sync {
    fn on_status(&self, status: ControllerStatus);
}

enum Command {
    Touch(TouchEvent),
    Global(GlobalAction),
    Type(String),
    Stop,
}

/// Controls the phone with the same access key until
/// [`stop`](Self::stop).
#[derive(uniffi::Object)]
pub struct ControllerSession {
    commands: mpsc::UnboundedSender<Command>,
    details: Arc<Mutex<Option<String>>>,
}

impl std::fmt::Debug for ControllerSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControllerSession").finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[uniffi::export]
impl ControllerSession {
    /// Starts controlling. `server` is the relay server setting; `name`
    /// names this device for the phone. Problems come to `listener` as
    /// statuses, never as errors here. On iPhone, set up the audio session
    /// (playback) before this.
    #[uniffi::constructor]
    pub fn start(
        server: String,
        access_key: String,
        name: String,
        listener: Arc<dyn ControllerListener>,
    ) -> Arc<Self> {
        let (commands, receiver) = mpsc::unbounded_channel();
        let details = Arc::new(Mutex::new(None));
        let task = Task {
            listener,
            name: peer::clean_name(&name),
            details: Arc::clone(&details),
            last_status: None,
        };
        runtime().spawn(task.run(server, access_key, receiver));
        Arc::new(Self { commands, details })
    }

    /// A change to the touch on this device. Sent in order; dropped while
    /// not connected to a phone.
    pub fn send_touch(&self, event: TouchEvent) {
        let _ = self.commands.send(Command::Touch(event));
    }

    pub fn send_global_action(&self, action: GlobalAction) {
        let _ = self.commands.send(Command::Global(action));
    }

    /// Types `text` into the phone's focused field, at the cursor.
    pub fn send_text(&self, text: String) {
        let _ = self.commands.send(Command::Type(text));
    }

    /// The audio stream's state, one "Label: value" per line, or `None`
    /// while there is no stream.
    pub fn audio_details(&self) -> Option<String> {
        lock(&self.details).clone()
    }

    /// Stops controlling and tells the phone.
    pub fn stop(&self) {
        let _ = self.commands.send(Command::Stop);
    }
}

enum Ended {
    Stopped,
    Lost(String),
    Fatal(String),
}

/// The connection to the phone, once it welcomed this device.
struct Session {
    host: u32,
    sealer: Sealer,
    sequence: Sequence,
}

/// A phone in the room.
struct Phone {
    id: u32,
    /// The nonce of the hello sent to it, until its welcome.
    nonce: Option<[u8; NONCE_LEN]>,
    session: Option<Session>,
    name: Option<String>,
    screen: Option<Screen>,
}

struct Audio {
    playback: Option<Playback>,
    key: [u8; 32],
    problem: Option<String>,
    /// Packets received at the last check, and when that number last grew.
    packets: u64,
    grew: Instant,
}

struct Task {
    listener: Arc<dyn ControllerListener>,
    name: String,
    details: Arc<Mutex<Option<String>>>,
    last_status: Option<ControllerStatus>,
}

/// How long without audio packets before the audio counts as waiting
/// again. The phone sends silence between sounds, so a gap is a problem.
const AUDIO_STALE: Duration = Duration::from_secs(2);

impl Task {
    fn report(
        &mut self,
        state: ControllerState,
        phone: Option<&Phone>,
        audio: Option<&Audio>,
        detail: Option<String>,
    ) {
        let connected = phone.is_some_and(|p| p.session.is_some());
        let audio_state = match audio {
            None => AudioState::Off,
            Some(a) if a.problem.is_some() => AudioState::Failed,
            Some(a) if a.packets > 0 && a.grew.elapsed() < AUDIO_STALE => AudioState::Playing,
            Some(_) => AudioState::Waiting,
        };
        let detail = detail.or_else(|| audio.and_then(|a| a.problem.clone()));
        let status = ControllerStatus {
            state,
            phone: phone.filter(|_| connected).and_then(|p| p.name.clone()),
            screen: phone.filter(|_| connected).and_then(|p| p.screen),
            audio: if connected {
                audio_state
            } else {
                AudioState::Off
            },
            detail,
        };
        if self.last_status.as_ref() != Some(&status) {
            self.last_status = Some(status.clone());
            self.listener.on_status(status);
        }
    }

    async fn run(
        mut self,
        server: String,
        access_key: String,
        mut commands: mpsc::UnboundedReceiver<Command>,
    ) {
        self.report(ControllerState::Connecting, None, None, None);
        let server = match Server::parse(&server) {
            Ok(s) => s,
            Err(e) => return self.report(ControllerState::Failed, None, None, Some(e)),
        };
        let keys = match tokio::task::spawn_blocking(move || Keys::derive(&access_key)).await {
            Ok(Ok(k)) => k,
            Ok(Err(problem)) => {
                return self.report(
                    ControllerState::Failed,
                    None,
                    None,
                    Some(problem.to_string()),
                );
            }
            Err(_) => {
                return self.report(
                    ControllerState::Failed,
                    None,
                    None,
                    Some("Could not prepare the access key.".into()),
                );
            }
        };
        let mut backoff = Backoff::default();
        loop {
            let ended = self
                .serve(&server, &keys, &mut commands, &mut backoff)
                .await;
            *lock(&self.details) = None;
            match ended {
                Ended::Stopped => return self.report(ControllerState::Stopped, None, None, None),
                Ended::Fatal(why) => {
                    return self.report(ControllerState::Failed, None, None, Some(why));
                }
                Ended::Lost(why) => {
                    self.report(ControllerState::Reconnecting, None, None, Some(why))
                }
            }
            let wait = backoff.delay();
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(wait) => break,
                    command = commands.recv() => match command {
                        // Touches while reconnecting go nowhere.
                        Some(Command::Touch(_) | Command::Global(_) | Command::Type(_)) => {}
                        Some(Command::Stop) | None => {
                            return self.report(ControllerState::Stopped, None, None, None);
                        }
                    },
                }
            }
        }
    }

    async fn hello(
        socket: &mut Socket,
        handshake: &Sealer,
        phone: &mut Phone,
        name: &str,
    ) -> Result<(), String> {
        let nonce = keys::nonce();
        phone.nonce = Some(nonce);
        phone.session = None;
        let hello = ToHost::Hello {
            version: PEER_VERSION,
            nonce: to_hex(&nonce),
            name: name.to_owned(),
        };
        let data = handshake.seal(
            Direction::ToHost,
            &serde_json::to_vec(&hello).expect("serializes"),
        );
        link::send(socket, &RelayRequest::Send { to: phone.id, data }).await
    }

    async fn serve(
        &mut self,
        server: &Server,
        keys: &Keys,
        commands: &mut mpsc::UnboundedReceiver<Command>,
        backoff: &mut Backoff,
    ) -> Ended {
        let (mut socket, joined) = match link::join(server, &keys.room, Role::Controller).await {
            Ok(j) => j,
            Err(e) => return Ended::Lost(e),
        };
        let handshake = Sealer::new(&keys.handshake(), Kind::Handshake);
        let mut phone: Option<Phone> =
            joined
                .peers
                .iter()
                .find(|p| p.role == Role::Host)
                .map(|p| Phone {
                    id: p.peer_id,
                    nonce: None,
                    session: None,
                    name: None,
                    screen: None,
                });
        if let Some(p) = phone.as_mut()
            && let Err(e) = Self::hello(&mut socket, &handshake, p, &self.name).await
        {
            return Ended::Lost(e);
        }
        let mut audio: Option<Audio> = None;
        self.report(ControllerState::WaitingForPhone, None, None, None);
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        let mut heard = Instant::now();
        let mut last_hello = Instant::now();
        let connected_at = Instant::now();
        loop {
            if connected_at.elapsed() > Duration::from_secs(30) {
                backoff.reset();
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
                        RelayEvent::PeerJoined { peer } if peer.role == Role::Host => {
                            // The phone came (back): a new connection.
                            audio = None;
                            *lock(&self.details) = None;
                            let p = phone.insert(Phone { id: peer.peer_id, nonce: None, session: None, name: None, screen: None });
                            if let Err(e) = Self::hello(&mut socket, &handshake, p, &self.name).await {
                                return Ended::Lost(e);
                            }
                        }
                        RelayEvent::PeerLeft { peer_id } if phone.as_ref().is_some_and(|p| p.id == peer_id) => {
                            phone = None;
                            audio = None;
                            *lock(&self.details) = None;
                        }
                        RelayEvent::Message { from, data } => {
                            if let Some(p) = phone.as_mut().filter(|p| p.id == from) {
                                match self.on_message(keys, &handshake, p, &data) {
                                    Some(Opened::Welcome(stream_nonce)) => {
                                        audio = Some(self.start_audio(server, &joined, keys.media(&stream_nonce)).await);
                                    }
                                    Some(Opened::Bye(reason)) => {
                                        phone = None;
                                        audio = None;
                                        *lock(&self.details) = None;
                                        self.report(ControllerState::WaitingForPhone, None, None, Some(reason));
                                        continue;
                                    }
                                    None => {}
                                }
                            }
                        }
                        RelayEvent::Error { code, message, fatal } => {
                            if code == ErrorCode::Version {
                                return Ended::Fatal(message);
                            }
                            if fatal {
                                return Ended::Lost(message);
                            }
                        }
                        _ => {}
                    }
                    self.report_now(phone.as_ref(), audio.as_ref());
                }
                command = commands.recv() => {
                    let message = match command {
                        Some(Command::Touch(event)) => Some(Message::Touch(event)),
                        Some(Command::Global(action)) => Some(Message::Global(action)),
                        Some(Command::Type(text)) => {
                            let text: String = text.chars().take(MAX_TYPED_CHARS).collect();
                            (!text.is_empty()).then_some(Message::Type(text))
                        }
                        Some(Command::Stop) | None => {
                            if let Some(s) = phone.as_mut().and_then(|p| p.session.as_mut()) {
                                let bye = ToHost::Bye { seq: s.sequence.next_out() };
                                let data = s.sealer.seal(Direction::ToHost, &serde_json::to_vec(&bye).expect("serializes"));
                                let _ = link::send(&mut socket, &RelayRequest::Send { to: s.host, data }).await;
                            }
                            let _ = socket.close(None).await;
                            return Ended::Stopped;
                        }
                    };
                    if let (Some(message), Some(s)) = (message, phone.as_mut().and_then(|p| p.session.as_mut())) {
                        let seq = s.sequence.next_out();
                        let body = match message {
                            Message::Touch(event) => ToHost::Touch { seq, event },
                            Message::Global(action) => ToHost::Global { seq, action },
                            Message::Type(text) => ToHost::Type { seq, text },
                        };
                        let data = s.sealer.seal(Direction::ToHost, &serde_json::to_vec(&body).expect("serializes"));
                        if let Err(e) = link::send(&mut socket, &RelayRequest::Send { to: s.host, data }).await {
                            return Ended::Lost(e);
                        }
                    }
                }
                _ = tick.tick() => {
                    if heard.elapsed() > RELAY_SILENCE {
                        return Ended::Lost("The relay server stopped answering.".into());
                    }
                    if let Some(a) = audio.as_mut() {
                        let restart = a.playback.as_ref().is_some_and(Playback::failed);
                        if restart {
                            // The output went away or changed (headphones,
                            // Bluetooth): start the stream again on the
                            // new default output.
                            a.playback = None;
                            let key = a.key;
                            *a = self.start_audio(server, &joined, key).await;
                        }
                        if let Some(p) = a.playback.as_ref() {
                            if last_hello.elapsed() >= Duration::from_millis(UDP_HELLO_INTERVAL_MS) {
                                p.keep_alive();
                                last_hello = Instant::now();
                            }
                            let packets = p.receiver_stats().snapshot().datagrams;
                            if packets > a.packets {
                                a.packets = packets;
                                a.grew = Instant::now();
                            }
                            *lock(&self.details) = Some(audio_out::describe(p.receiver_stats(), p.playout_stats()));
                        }
                    }
                    self.report_now(phone.as_ref(), audio.as_ref());
                }
            }
        }
    }

    fn report_now(&mut self, phone: Option<&Phone>, audio: Option<&Audio>) {
        let state = if phone.is_some_and(|p| p.session.is_some()) {
            ControllerState::Connected
        } else {
            ControllerState::WaitingForPhone
        };
        // A goodbye's reason stays until something changes.
        let detail = self
            .last_status
            .as_ref()
            .filter(|s| s.state == state && s.audio != AudioState::Failed)
            .and_then(|s| s.detail.clone());
        self.report(state, phone, audio, detail);
    }

    async fn start_audio(&self, server: &Server, joined: &Joined, key: [u8; 32]) -> Audio {
        let server = server.clone();
        let joined = joined.clone();
        let started = tokio::task::spawn_blocking(move || {
            let udp = Udp::open(&server, &joined)?;
            Playback::start(udp, key)
        })
        .await
        .unwrap_or_else(|_| Err("The audio thread stopped unexpectedly.".into()));
        let (playback, problem) = match started {
            Ok(p) => (Some(p), None),
            Err(e) => (None, Some(format!("Audio cannot play: {e}"))),
        };
        Audio {
            playback,
            key,
            problem,
            packets: 0,
            grew: Instant::now(),
        }
    }

    fn on_message(
        &mut self,
        keys: &Keys,
        handshake: &Sealer,
        phone: &mut Phone,
        data: &str,
    ) -> Option<Opened> {
        match seal::kind_of(data)? {
            Kind::Handshake => {
                let nonce = phone.nonce?;
                let plain = handshake.open(Direction::ToController, data)?;
                let ToController::Welcome {
                    version,
                    controller_nonce,
                    host_nonce,
                    stream_nonce,
                    name,
                    screen,
                } = serde_json::from_slice(&plain).ok()?
                else {
                    return None;
                };
                if version != PEER_VERSION || from_hex::<NONCE_LEN>(&controller_nonce)? != nonce {
                    return None;
                }
                let host_nonce = from_hex::<NONCE_LEN>(&host_nonce)?;
                let stream_nonce = from_hex::<NONCE_LEN>(&stream_nonce)?;
                phone.nonce = None;
                phone.session = Some(Session {
                    host: phone.id,
                    sealer: Sealer::new(&keys.session(&nonce, &host_nonce), Kind::Session),
                    sequence: Sequence::default(),
                });
                phone.name = Some(peer::clean_name(&name));
                phone.screen = Some(screen);
                Some(Opened::Welcome(stream_nonce))
            }
            Kind::Session => {
                let session = phone.session.as_mut()?;
                let plain = session.sealer.open(Direction::ToController, data)?;
                let message: ToController = serde_json::from_slice(&plain).ok()?;
                if !session.sequence.accept(message.seq()?) {
                    return None;
                }
                match message {
                    ToController::Screen { screen, .. } => {
                        phone.screen = Some(screen);
                        None
                    }
                    ToController::Bye { reason, .. } => {
                        Some(Opened::Bye(peer::clean_name(&reason)))
                    }
                    ToController::Welcome { .. } => None,
                }
            }
        }
    }
}

enum Message {
    Touch(TouchEvent),
    Global(GlobalAction),
    Type(String),
}

enum Opened {
    Welcome([u8; NONCE_LEN]),
    Bye(String),
}
