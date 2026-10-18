//! The Backtalk remote access relay.
//!
//! Apps meet in rooms named by a hash of their access key. The relay passes
//! sealed messages between a room's host (the phone being controlled) and
//! its controllers, and copies the host's UDP media to every controller. It
//! holds no keys and keeps nothing on disk: a room exists while someone is
//! in it.
//!
//! Limits keep one client from starving the rest: connections per address,
//! controllers per room, messages per second, and datagrams per second.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::get;
use backtalk_remote_protocol::{
    ErrorCode, MAX_DATAGRAM, MAX_MESSAGE_DATA, PROTOCOL_VERSION, Peer, RelayEvent, RelayRequest,
    Role, UDP_HELLO_MAGIC, UDP_TOKEN_LEN, is_room, to_hex,
};
use futures_util::StreamExt;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

/// Relay limits.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub connections_per_ip: usize,
    pub controllers_per_room: usize,
    pub messages_per_second: u32,
    pub datagrams_per_second: u32,
    /// How long a new connection has to join a room.
    pub join_timeout: Duration,
    /// How long a connection may stay silent (no message, no pong).
    pub idle_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            connections_per_ip: 32,
            controllers_per_room: 8,
            // Touch moves come up to about 120 a second.
            messages_per_second: 400,
            // AudioNet sends a 10 ms frame per datagram: 100 a second.
            datagrams_per_second: 400,
            join_timeout: Duration::from_secs(15),
            idle_timeout: Duration::from_secs(60),
        }
    }
}

/// How often the relay pings each WebSocket.
const PING_INTERVAL: Duration = Duration::from_secs(20);
/// Events waiting for a slow client before it is disconnected.
const OUTBOX: usize = 512;

struct Member {
    role: Role,
    outbox: mpsc::Sender<Outgoing>,
    token: [u8; UDP_TOKEN_LEN],
    udp: Option<SocketAddr>,
    /// Datagram budget for the current second.
    window: Instant,
    datagrams: u32,
}

#[derive(Default)]
struct Room {
    members: HashMap<u32, Member>,
}

impl Room {
    fn host(&self) -> Option<u32> {
        self.members
            .iter()
            .find(|(_, m)| m.role == Role::Host)
            .map(|(id, _)| *id)
    }

    fn controllers(&self) -> usize {
        self.members
            .values()
            .filter(|m| m.role == Role::Controller)
            .count()
    }
}

#[derive(Default)]
struct Rooms {
    rooms: HashMap<String, Room>,
    /// UDP token → (room, peer).
    tokens: HashMap<[u8; UDP_TOKEN_LEN], (String, u32)>,
    /// UDP address → (room, peer).
    addresses: HashMap<SocketAddr, (String, u32)>,
    connections: HashMap<IpAddr, usize>,
}

enum Outgoing {
    Event(RelayEvent),
    /// A fatal error: sent, then the connection closes.
    Close(RelayEvent),
}

/// The relay's shared state.
pub struct Relay {
    rooms: Mutex<Rooms>,
    next_peer: AtomicU32,
    udp: Arc<UdpSocket>,
    udp_port: u16,
    limits: Limits,
}

impl std::fmt::Debug for Relay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Relay")
            .field("udp_port", &self.udp_port)
            .finish_non_exhaustive()
    }
}

impl Relay {
    /// A relay forwarding media on `udp`, telling apps it is on `udp_port`
    /// (the public port, if a firewall maps another).
    pub fn new(udp: UdpSocket, udp_port: u16, limits: Limits) -> Arc<Self> {
        Arc::new(Self {
            rooms: Mutex::new(Rooms::default()),
            next_peer: AtomicU32::new(1),
            udp: Arc::new(udp),
            udp_port,
            limits,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Rooms> {
        self.rooms.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The web routes: the WebSocket and the health check.
    pub fn router(self: &Arc<Self>) -> Router {
        Router::new()
            .route(backtalk_remote_protocol::WEBSOCKET_PATH, get(websocket))
            .route(backtalk_remote_protocol::HEALTH_PATH, get(health))
            .with_state(Arc::clone(self))
    }

    /// Forwards media until the socket fails. Run it as a task.
    pub async fn run_udp(self: Arc<Self>) {
        let mut buf = vec![0u8; 2048];
        let mut targets: Vec<SocketAddr> = Vec::with_capacity(8);
        loop {
            let (n, from) = match self.udp.recv_from(&mut buf).await {
                Ok(r) => r,
                Err(e) => {
                    // ICMP errors from a peer that went away surface here on
                    // some systems; they are not fatal.
                    tracing::debug!("UDP receive: {e}");
                    continue;
                }
            };
            let datagram = &buf[..n];
            if datagram.len() == UDP_HELLO_MAGIC.len() + UDP_TOKEN_LEN
                && datagram.starts_with(UDP_HELLO_MAGIC)
            {
                self.on_hello(&datagram[UDP_HELLO_MAGIC.len()..], from);
                continue;
            }
            if n > MAX_DATAGRAM {
                continue;
            }
            targets.clear();
            self.media_targets(from, &mut targets);
            for to in &targets {
                // A full socket buffer drops the datagram: media is
                // real-time, and AudioNet's receiver conceals the loss.
                let _ = self.udp.try_send_to(datagram, *to);
            }
        }
    }

    fn on_hello(&self, token: &[u8], from: SocketAddr) {
        let Ok(token) = <[u8; UDP_TOKEN_LEN]>::try_from(token) else {
            return;
        };
        let mut rooms = self.lock();
        let Some((room_name, peer_id)) = rooms.tokens.get(&token).cloned() else {
            return;
        };
        let Some(member) = rooms
            .rooms
            .get_mut(&room_name)
            .and_then(|r| r.members.get_mut(&peer_id))
        else {
            return;
        };
        let previous = member.udp.replace(from);
        let outbox = member.outbox.clone();
        if previous != Some(from) {
            if let Some(old) = previous {
                rooms.addresses.remove(&old);
            }
            rooms.addresses.insert(from, (room_name, peer_id));
            let _ = outbox.try_send(Outgoing::Event(RelayEvent::UdpReady));
        }
    }

    /// Where a datagram from `from` goes: a host's media to every
    /// controller in its room that has a UDP address. Controllers send no
    /// media yet, so theirs goes nowhere.
    fn media_targets(&self, from: SocketAddr, targets: &mut Vec<SocketAddr>) {
        let mut rooms = self.lock();
        let Some((room_name, peer_id)) = rooms.addresses.get(&from).cloned() else {
            return;
        };
        let limit = self.limits.datagrams_per_second;
        let Some(room) = rooms.rooms.get_mut(&room_name) else {
            return;
        };
        let Some(sender) = room.members.get_mut(&peer_id) else {
            return;
        };
        if sender.role != Role::Host
            || !within_budget(&mut sender.window, &mut sender.datagrams, limit)
        {
            return;
        }
        targets.extend(
            room.members
                .values()
                .filter(|m| m.role == Role::Controller)
                .filter_map(|m| m.udp),
        );
    }

    fn join(
        &self,
        room_name: &str,
        role: Role,
        outbox: mpsc::Sender<Outgoing>,
    ) -> Result<(u32, RelayEvent), RelayEvent> {
        let mut rooms = self.lock();
        let rooms = &mut *rooms;
        let room = rooms.rooms.entry(room_name.to_owned()).or_default();
        if role == Role::Controller && room.controllers() >= self.limits.controllers_per_room {
            return Err(error(
                ErrorCode::Busy,
                "This phone already has as many controllers as it can take.",
                true,
            ));
        }
        if role == Role::Host
            && let Some(old) = room.host()
            && let Some(member) = room.members.remove(&old)
        {
            // The newest host wins: usually the same phone, reconnecting
            // before the relay noticed its old connection drop.
            forget_member(&mut rooms.tokens, &mut rooms.addresses, &member);
            let _ = member.outbox.try_send(Outgoing::Close(error(
                ErrorCode::Replaced,
                "Another phone started sharing with this access key.",
                true,
            )));
            broadcast(room, old, RelayEvent::PeerLeft { peer_id: old });
        }
        let peer_id = self.next_peer.fetch_add(1, Relaxed);
        let mut token = [0u8; UDP_TOKEN_LEN];
        rand::fill(&mut token);
        let peers = room
            .members
            .iter()
            .map(|(id, m)| Peer {
                peer_id: *id,
                role: m.role,
            })
            .collect();
        broadcast(
            room,
            peer_id,
            RelayEvent::PeerJoined {
                peer: Peer { peer_id, role },
            },
        );
        room.members.insert(
            peer_id,
            Member {
                role,
                outbox,
                token,
                udp: None,
                window: Instant::now(),
                datagrams: 0,
            },
        );
        rooms.tokens.insert(token, (room_name.to_owned(), peer_id));
        Ok((
            peer_id,
            RelayEvent::Joined {
                version: PROTOCOL_VERSION,
                peer_id,
                udp_port: self.udp_port,
                udp_token: to_hex(&token),
                peers,
            },
        ))
    }

    fn leave(&self, room_name: &str, peer_id: u32) {
        let mut rooms = self.lock();
        let rooms = &mut *rooms;
        let Some(room) = rooms.rooms.get_mut(room_name) else {
            return;
        };
        let Some(member) = room.members.remove(&peer_id) else {
            return;
        };
        forget_member(&mut rooms.tokens, &mut rooms.addresses, &member);
        broadcast(room, peer_id, RelayEvent::PeerLeft { peer_id });
        if room.members.is_empty() {
            rooms.rooms.remove(room_name);
        }
    }

    /// Passes `data` from `from` to `to`, if they are in the same room and
    /// one is the host.
    fn forward(&self, room_name: &str, from: u32, to: u32, data: String) -> Result<(), RelayEvent> {
        let rooms = self.lock();
        let room = rooms.rooms.get(room_name);
        let sender = room.and_then(|r| r.members.get(&from));
        let target = room.and_then(|r| r.members.get(&to));
        match (sender, target) {
            (Some(s), Some(t)) if s.role != t.role => {
                if t.outbox
                    .try_send(Outgoing::Event(RelayEvent::Message { from, data }))
                    .is_err()
                {
                    // The other app is not keeping up with OUTBOX events
                    // waiting: the message is dropped rather than held.
                    tracing::debug!("outbox full for peer {to}");
                }
                Ok(())
            }
            _ => Err(error(
                ErrorCode::NoPeer,
                "That device is no longer connected.",
                false,
            )),
        }
    }

    fn connect(&self, ip: IpAddr) -> bool {
        let mut rooms = self.lock();
        let count = rooms.connections.entry(ip).or_default();
        if *count >= self.limits.connections_per_ip {
            return false;
        }
        *count += 1;
        true
    }

    fn disconnect(&self, ip: IpAddr) {
        let mut rooms = self.lock();
        if let Some(count) = rooms.connections.get_mut(&ip) {
            *count -= 1;
            if *count == 0 {
                rooms.connections.remove(&ip);
            }
        }
    }

    /// Rooms and members now, for the health check.
    fn counts(&self) -> (usize, usize) {
        let rooms = self.lock();
        let members = rooms.rooms.values().map(|r| r.members.len()).sum();
        (rooms.rooms.len(), members)
    }
}

fn forget_member(
    tokens: &mut HashMap<[u8; UDP_TOKEN_LEN], (String, u32)>,
    addresses: &mut HashMap<SocketAddr, (String, u32)>,
    member: &Member,
) {
    tokens.remove(&member.token);
    if let Some(addr) = member.udp {
        addresses.remove(&addr);
    }
}

fn broadcast(room: &Room, except: u32, event: RelayEvent) {
    for (id, m) in &room.members {
        if *id != except {
            let _ = m.outbox.try_send(Outgoing::Event(event.clone()));
        }
    }
}

fn error(code: ErrorCode, message: &str, fatal: bool) -> RelayEvent {
    RelayEvent::Error {
        code,
        message: message.to_owned(),
        fatal,
    }
}

/// Counts one more event in a one-second window; false once `limit` is
/// passed.
fn within_budget(window: &mut Instant, count: &mut u32, limit: u32) -> bool {
    let now = Instant::now();
    if now.duration_since(*window) >= Duration::from_secs(1) {
        *window = now;
        *count = 0;
    }
    *count += 1;
    *count <= limit
}

async fn health(State(relay): State<Arc<Relay>>) -> impl IntoResponse {
    let (rooms, members) = relay.counts();
    format!(
        "Backtalk remote relay\nProtocol: {PROTOCOL_VERSION}\nRooms: {rooms}\nConnected: {members}\n"
    )
}

/// The client's address: the proxy's `X-Real-IP` when the connection comes
/// from the proxy on this machine, else the connection's own.
fn client_ip(peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
    if peer.ip().is_loopback()
        && let Some(ip) = headers
            .get("x-real-ip")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse().ok())
    {
        return ip;
    }
    peer.ip()
}

async fn websocket(
    State(relay): State<Arc<Relay>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> impl IntoResponse {
    let ip = client_ip(peer, &headers);
    upgrade
        .max_message_size(MAX_MESSAGE_DATA + 1024)
        .on_upgrade(move |socket| async move {
            if !relay.connect(ip) {
                let mut socket = socket;
                let _ = send_event(
                    &mut socket,
                    &error(
                        ErrorCode::Busy,
                        "Too many connections from your network.",
                        true,
                    ),
                )
                .await;
                return;
            }
            serve(&relay, socket).await;
            relay.disconnect(ip);
        })
}

async fn send_event(socket: &mut WebSocket, event: &RelayEvent) -> Result<(), axum::Error> {
    let text = serde_json::to_string(event).expect("events serialize");
    socket.send(Message::Text(text.into())).await
}

/// One app's connection: wait for its join, then pass messages until it
/// leaves.
async fn serve(relay: &Arc<Relay>, mut socket: WebSocket) {
    let join = tokio::time::timeout(relay.limits.join_timeout, socket.next()).await;
    let Ok(Some(Ok(Message::Text(text)))) = join else {
        return;
    };
    let (room_name, role) = match serde_json::from_str::<RelayRequest>(&text) {
        Ok(RelayRequest::Join {
            version,
            room,
            role,
        }) => {
            if version != PROTOCOL_VERSION {
                let _ = send_event(
                    &mut socket,
                    &error(
                        ErrorCode::Version,
                        "This app and the relay server speak different versions. Update the app.",
                        true,
                    ),
                )
                .await;
                return;
            }
            if !is_room(&room) {
                let _ = send_event(
                    &mut socket,
                    &error(ErrorCode::BadRequest, "The room name is not valid.", true),
                )
                .await;
                return;
            }
            (room, role)
        }
        _ => {
            let _ = send_event(
                &mut socket,
                &error(
                    ErrorCode::BadRequest,
                    "The first message must join a room.",
                    true,
                ),
            )
            .await;
            return;
        }
    };
    let (outbox, mut inbox) = mpsc::channel(OUTBOX);
    let (peer_id, joined) = match relay.join(&room_name, role, outbox) {
        Ok(r) => r,
        Err(e) => {
            let _ = send_event(&mut socket, &e).await;
            return;
        }
    };
    tracing::info!("peer {peer_id} joined as {role:?}");
    if send_event(&mut socket, &joined).await.is_ok() {
        pump(relay, &mut socket, &room_name, peer_id, &mut inbox).await;
    }
    relay.leave(&room_name, peer_id);
    tracing::info!("peer {peer_id} left");
}

async fn pump(
    relay: &Arc<Relay>,
    socket: &mut WebSocket,
    room_name: &str,
    peer_id: u32,
    inbox: &mut mpsc::Receiver<Outgoing>,
) {
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.tick().await;
    let mut heard = Instant::now();
    let mut window = Instant::now();
    let mut messages = 0u32;
    loop {
        tokio::select! {
            incoming = socket.next() => {
                let Some(Ok(message)) = incoming else { return };
                heard = Instant::now();
                match message {
                    Message::Text(text) => {
                        if !within_budget(&mut window, &mut messages, relay.limits.messages_per_second) {
                            let _ = send_event(socket, &error(ErrorCode::Busy, "Too many messages.", true)).await;
                            return;
                        }
                        match serde_json::from_str::<RelayRequest>(&text) {
                            Ok(RelayRequest::Send { to, data }) if data.len() <= MAX_MESSAGE_DATA => {
                                if let Err(e) = relay.forward(room_name, peer_id, to, data)
                                    && send_event(socket, &e).await.is_err()
                                {
                                    return;
                                }
                            }
                            _ => {
                                let _ = send_event(socket, &error(ErrorCode::BadRequest, "That message could not be read.", true)).await;
                                return;
                            }
                        }
                    }
                    Message::Close(_) => return,
                    _ => {}
                }
            }
            outgoing = inbox.recv() => {
                match outgoing {
                    Some(Outgoing::Event(event)) => {
                        if send_event(socket, &event).await.is_err() {
                            return;
                        }
                    }
                    Some(Outgoing::Close(event)) => {
                        let _ = send_event(socket, &event).await;
                        return;
                    }
                    None => return,
                }
            }
            _ = ping.tick() => {
                if heard.elapsed() > relay.limits.idle_timeout {
                    return;
                }
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
