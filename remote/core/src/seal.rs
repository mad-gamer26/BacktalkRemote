//! Sealing what the devices say to each other through the relay.
//!
//! ```text
//! +------+-------------+------------------------------+----------+
//! | kind | nonce (24 B)| encrypted JSON                | tag (16) |
//! +------+-------------+------------------------------+----------+
//! ```
//!
//! XChaCha20-Poly1305 with a random 192-bit nonce per message, as AudioNet
//! uses for media. The kind byte (handshake or session) and the direction
//! are authenticated, so a message cannot be reflected back to its sender
//! or moved from the handshake to a session. Replays within a session are
//! dropped by sequence number (see [`crate::peer`]).

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};

use crate::keys::KEY_LEN;

const NONCE_LEN: usize = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    /// Hello and welcome, under the handshake key.
    Handshake = 1,
    /// Everything after, under the connection's session key.
    Session = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    ToHost,
    ToController,
}

impl Direction {
    fn label(self) -> &'static [u8] {
        match self {
            Direction::ToHost => b"to host",
            Direction::ToController => b"to controller",
        }
    }
}

pub struct Sealer {
    aead: XChaCha20Poly1305,
    kind: Kind,
}

impl std::fmt::Debug for Sealer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sealer")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

fn aad(kind: Kind, direction: Direction) -> Vec<u8> {
    let mut a = b"backtalk-remote/1/".to_vec();
    a.push(kind as u8);
    a.extend_from_slice(direction.label());
    a
}

impl Sealer {
    pub fn new(key: &[u8; KEY_LEN], kind: Kind) -> Self {
        Self {
            aead: XChaCha20Poly1305::new_from_slice(key).expect("32-byte key"),
            kind,
        }
    }

    /// Seals `plain` for `direction`, as base64 for the relay.
    pub fn seal(&self, direction: Direction, plain: &[u8]) -> String {
        let mut nonce = [0u8; NONCE_LEN];
        rand::fill(&mut nonce);
        let sealed = self
            .aead
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: plain,
                    aad: &aad(self.kind, direction),
                },
            )
            .expect("encryption with a valid key does not fail");
        let mut out = Vec::with_capacity(1 + NONCE_LEN + sealed.len());
        out.push(self.kind as u8);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&sealed);
        STANDARD.encode(out)
    }

    /// Opens a message sealed for `direction` with this key and kind, or
    /// `None` if it is not one.
    pub fn open(&self, direction: Direction, data: &str) -> Option<Vec<u8>> {
        let bytes = STANDARD.decode(data).ok()?;
        if bytes.len() < 1 + NONCE_LEN || bytes[0] != self.kind as u8 {
            return None;
        }
        self.aead
            .decrypt(
                &XNonce::try_from(&bytes[1..1 + NONCE_LEN]).ok()?,
                Payload {
                    msg: &bytes[1 + NONCE_LEN..],
                    aad: &aad(self.kind, direction),
                },
            )
            .ok()
    }
}

/// The kind byte of a message from the relay, without opening it.
pub fn kind_of(data: &str) -> Option<Kind> {
    // The first base64 character carries the top six bits of the kind
    // byte; decode just the first four characters (three bytes).
    let head = STANDARD.decode(data.get(..4)?).ok()?;
    match head.first()? {
        1 => Some(Kind::Handshake),
        2 => Some(Kind::Session),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_messages_open_only_with_the_same_key_kind_and_direction() {
        let key = [7u8; KEY_LEN];
        let s = Sealer::new(&key, Kind::Session);
        let data = s.seal(Direction::ToHost, b"hello");
        assert_eq!(kind_of(&data), Some(Kind::Session));
        assert_eq!(
            s.open(Direction::ToHost, &data).as_deref(),
            Some(&b"hello"[..])
        );
        assert_eq!(s.open(Direction::ToController, &data), None);
        assert_eq!(
            Sealer::new(&key, Kind::Handshake).open(Direction::ToHost, &data),
            None
        );
        assert_eq!(
            Sealer::new(&[8u8; KEY_LEN], Kind::Session).open(Direction::ToHost, &data),
            None
        );
    }

    #[test]
    fn tampering_and_garbage_are_refused() {
        let s = Sealer::new(&[1u8; KEY_LEN], Kind::Handshake);
        let data = s.seal(Direction::ToController, b"welcome");
        let mut bytes = STANDARD.decode(&data).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        assert_eq!(
            s.open(Direction::ToController, &STANDARD.encode(bytes)),
            None
        );
        assert_eq!(s.open(Direction::ToController, "not base64!"), None);
        assert_eq!(s.open(Direction::ToController, ""), None);
        assert_eq!(kind_of("x"), None);
    }

    #[test]
    fn the_same_message_seals_differently_each_time() {
        let s = Sealer::new(&[2u8; KEY_LEN], Kind::Session);
        assert_ne!(
            s.seal(Direction::ToHost, b"a"),
            s.seal(Direction::ToHost, b"a")
        );
    }
}
