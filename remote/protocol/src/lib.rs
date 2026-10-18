//! The messages between Backtalk remote access apps and the relay.
//!
//! An app holds one WebSocket to the relay (text frames, JSON) and one UDP
//! socket. The relay never sees the access key: a room is named by a hash
//! of it, and everything one app says to another passes through the relay
//! sealed (`data`, base64), so the relay only forwards. Media goes over
//! UDP: the host's datagrams (AudioNet RTP, encrypted) are copied to every
//! controller in the room.
//!
//! JSON fields are `snake_case`, always present, `null` when absent.
//! [`PROTOCOL_VERSION`] changes when either side would misread the other.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// The relay protocol version.
pub const PROTOCOL_VERSION: u32 = 1;

/// The WebSocket path on the relay's web server.
pub const WEBSOCKET_PATH: &str = "/backtalk-remote/ws";

/// The relay's health check, for monitoring and for installers.
pub const HEALTH_PATH: &str = "/backtalk-remote/health";

/// A UDP datagram that starts with these bytes, followed by the 16-byte
/// token from [`RelayEvent::Joined`], tells the relay which address the
/// app's UDP socket has. The relay forwards no hello. Apps send one every
/// [`UDP_HELLO_INTERVAL_MS`] so that their NAT keeps the mapping open.
pub const UDP_HELLO_MAGIC: &[u8; 4] = b"BTRH";

/// Length of the UDP token.
pub const UDP_TOKEN_LEN: usize = 16;

/// How often apps repeat the UDP hello.
pub const UDP_HELLO_INTERVAL_MS: u64 = 10_000;

/// Largest datagram the relay forwards. AudioNet's media packets are well
/// under this (RTP header, 24-byte nonce, Opus, 16-byte tag).
pub const MAX_DATAGRAM: usize = 1_400;

/// Largest `data` (base64) the relay forwards in one message.
pub const MAX_MESSAGE_DATA: usize = 64 * 1024;

/// What an app is in a room: the phone being controlled, or a device
/// controlling it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Host,
    Controller,
}

/// Another app in the room.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peer {
    pub peer_id: u32,
    pub role: Role,
}

/// From an app to the relay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RelayRequest {
    /// The first message: enter the room. `room` is 64 hexadecimal digits.
    /// A host that joins a room that has a host replaces it (a phone whose
    /// connection dropped comes back before the relay notices).
    Join {
        version: u32,
        room: String,
        role: Role,
    },
    /// Pass `data` to `to`, a host to a controller or a controller to the
    /// host.
    Send { to: u32, data: String },
}

/// From the relay to an app.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RelayEvent {
    /// The app is in the room. `udp_port` is the relay's UDP port, on the
    /// same host name as the WebSocket; `udp_token` is 32 hexadecimal
    /// digits. `peers` are those already there.
    Joined {
        version: u32,
        peer_id: u32,
        udp_port: u16,
        udp_token: String,
        peers: Vec<Peer>,
    },
    PeerJoined {
        peer: Peer,
    },
    PeerLeft {
        peer_id: u32,
    },
    /// `data` from another app.
    Message {
        from: u32,
        data: String,
    },
    /// The relay has the app's UDP address (after its first hello).
    UdpReady,
    /// Something went wrong. `code` is for programs; `message` is in words.
    /// The relay closes the connection after a fatal error.
    Error {
        code: ErrorCode,
        message: String,
        fatal: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The app speaks another protocol version.
    Version,
    /// The message could not be read, or came at the wrong time.
    BadRequest,
    /// Another phone took over as host of this room.
    Replaced,
    /// No such peer, or not one this app may talk to.
    NoPeer,
    /// Too many messages, rooms or connections.
    Busy,
}

/// Whether `room` is a room name: 64 lowercase hexadecimal digits.
pub fn is_room(room: &str) -> bool {
    room.len() == 64 && room.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Encodes bytes as lowercase hexadecimal.
pub fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[usize::from(b >> 4)] as char);
        s.push(DIGITS[usize::from(b & 15)] as char);
    }
    s
}

/// Decodes lowercase or uppercase hexadecimal into exactly `N` bytes.
pub fn from_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    let t = text.as_bytes();
    if t.len() != N * 2 {
        return None;
    }
    let digit = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = (digit(t[2 * i])? << 4) | digit(t[2 * i + 1])?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_is_tagged_snake_case() {
        let join = RelayRequest::Join {
            version: 1,
            room: "ab".repeat(32),
            role: Role::Controller,
        };
        let text = serde_json::to_string(&join).unwrap();
        assert_eq!(
            text,
            format!(
                r#"{{"type":"join","version":1,"room":"{}","role":"controller"}}"#,
                "ab".repeat(32)
            )
        );
        assert_eq!(serde_json::from_str::<RelayRequest>(&text).unwrap(), join);
    }

    #[test]
    fn events_round_trip() {
        let events = [
            RelayEvent::Joined {
                version: 1,
                peer_id: 3,
                udp_port: 39861,
                udp_token: "00".repeat(16),
                peers: vec![Peer {
                    peer_id: 1,
                    role: Role::Host,
                }],
            },
            RelayEvent::PeerLeft { peer_id: 2 },
            RelayEvent::UdpReady,
            RelayEvent::Error {
                code: ErrorCode::Replaced,
                message: "Another phone".into(),
                fatal: true,
            },
        ];
        for e in events {
            let text = serde_json::to_string(&e).unwrap();
            assert_eq!(serde_json::from_str::<RelayEvent>(&text).unwrap(), e);
        }
    }

    #[test]
    fn rooms_are_64_lowercase_hex_digits() {
        assert!(is_room(&"0a".repeat(32)));
        assert!(!is_room(&"0A".repeat(32)));
        assert!(!is_room(&"0a".repeat(31)));
        assert!(!is_room(&"zz".repeat(32)));
    }

    #[test]
    fn hex_round_trips() {
        let bytes = [0u8, 1, 0xab, 0xff];
        assert_eq!(to_hex(&bytes), "0001abff");
        assert_eq!(from_hex::<4>("0001ABff"), Some(bytes));
        assert_eq!(from_hex::<4>("0001ab"), None);
        assert_eq!(from_hex::<2>("zz00"), None);
    }
}
