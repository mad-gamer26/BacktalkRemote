//! Keys made from the access key.
//!
//! Both devices are given the same access key, as in NVDA Remote. It is
//! stretched once with Argon2id, which makes each guess cost a fraction of
//! a second and a few megabytes, and the result is split with HKDF:
//!
//! * the **room**, the only thing the relay sees, so that devices with the
//!   same key find each other;
//! * the **master** secret, never sent anywhere, from which come the
//!   handshake key, each connection's session key (from both sides'
//!   random nonces) and each audio stream's media key.
//!
//! A wrong key is a different room, so a controller with a wrong key just
//! finds no phone.

use argon2::{Algorithm, Argon2, Params, Version};
use hkdf::Hkdf;
use sha2::Sha256;

pub const KEY_LEN: usize = 32;
pub const NONCE_LEN: usize = 16;

/// Shortest and longest access key, in characters.
pub const MIN_KEY_CHARS: usize = 8;
pub const MAX_KEY_CHARS: usize = 128;

const ARGON2_SALT: &[u8] = b"Backtalk remote access key v1";
// OWASP's second Argon2id configuration: 19 MiB, 2 passes, 1 lane. A few
// hundred milliseconds on a phone, once per connection.
const ARGON2_MEMORY_KIB: u32 = 19 * 1024;
const ARGON2_PASSES: u32 = 2;

/// What is wrong with an access key, in words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyProblem {
    TooShort,
    TooLong,
}

impl std::fmt::Display for KeyProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeyProblem::TooShort => write!(
                f,
                "The access key needs at least {MIN_KEY_CHARS} characters."
            ),
            KeyProblem::TooLong => write!(
                f,
                "The access key can have at most {MAX_KEY_CHARS} characters."
            ),
        }
    }
}

/// The access key as both devices use it: spaces around it are ignored;
/// inside it, case and spaces count.
pub fn normalize(key: &str) -> Result<&str, KeyProblem> {
    let key = key.trim();
    let chars = key.chars().count();
    if chars < MIN_KEY_CHARS {
        Err(KeyProblem::TooShort)
    } else if chars > MAX_KEY_CHARS {
        Err(KeyProblem::TooLong)
    } else {
        Ok(key)
    }
}

/// The room and master secret of an access key.
#[derive(Clone)]
pub struct Keys {
    pub room: String,
    master: [u8; KEY_LEN],
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keys")
            .field("room", &self.room)
            .finish_non_exhaustive()
    }
}

impl Keys {
    /// Stretches `key`. Slow on purpose: never on a UI or audio thread.
    pub fn derive(key: &str) -> Result<Keys, KeyProblem> {
        let key = normalize(key)?;
        let params = Params::new(ARGON2_MEMORY_KIB, ARGON2_PASSES, 1, Some(KEY_LEN))
            .expect("valid Argon2 parameters");
        let mut stretched = [0u8; KEY_LEN];
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(key.as_bytes(), ARGON2_SALT, &mut stretched)
            .expect("Argon2 accepts this key and salt");
        let hk = Hkdf::<Sha256>::new(None, &stretched);
        let mut room = [0u8; KEY_LEN];
        hk.expand(b"backtalk-remote room", &mut room)
            .expect("valid length");
        let mut master = [0u8; KEY_LEN];
        hk.expand(b"backtalk-remote master", &mut master)
            .expect("valid length");
        Ok(Keys {
            room: backtalk_remote_protocol::to_hex(&room),
            master,
        })
    }

    fn expand(&self, salt: Option<&[u8]>, info: &[u8]) -> [u8; KEY_LEN] {
        let hk = Hkdf::<Sha256>::new(salt, &self.master);
        let mut out = [0u8; KEY_LEN];
        hk.expand(info, &mut out).expect("valid length");
        out
    }

    /// Seals the hello and welcome that start each connection.
    pub fn handshake(&self) -> [u8; KEY_LEN] {
        self.expand(None, b"backtalk-remote handshake")
    }

    /// One controller's connection: both sides' nonces, so neither side
    /// alone picks the key, and no two connections share one.
    pub fn session(&self, controller: &[u8; NONCE_LEN], host: &[u8; NONCE_LEN]) -> [u8; KEY_LEN] {
        let mut salt = [0u8; NONCE_LEN * 2];
        salt[..NONCE_LEN].copy_from_slice(controller);
        salt[NONCE_LEN..].copy_from_slice(host);
        self.expand(Some(&salt), b"backtalk-remote session")
    }

    /// The audio stream's pre-shared key for AudioNet's media cipher.
    pub fn media(&self, stream: &[u8; NONCE_LEN]) -> [u8; KEY_LEN] {
        self.expand(Some(stream), b"backtalk-remote media")
    }
}

/// A random nonce.
pub fn nonce() -> [u8; NONCE_LEN] {
    let mut n = [0u8; NONCE_LEN];
    rand::fill(&mut n);
    n
}

/// Letters and digits that are hard to mistake for each other when read
/// aloud or typed: no 0/o, 1/l/i.
const KEY_ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";

/// A new access key: four groups of four characters, about 79 bits, such as
/// `kq7m-2xpr-9tdf-h4wa`.
pub fn generate_key() -> String {
    let mut bytes = [0u8; 16];
    rand::fill(&mut bytes);
    let mut key = String::with_capacity(19);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && i % 4 == 0 {
            key.push('-');
        }
        // 256 is not a multiple of 31; the bias (at most 1 in 8 more
        // likely for the first 8 characters) costs well under a bit.
        key.push(KEY_ALPHABET[usize::from(*b) % KEY_ALPHABET.len()] as char);
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_key_makes_the_same_room_and_keys() {
        let a = Keys::derive("  correct horse  ").unwrap();
        let b = Keys::derive("correct horse").unwrap();
        assert_eq!(a.room, b.room);
        assert_eq!(a.handshake(), b.handshake());
        assert!(backtalk_remote_protocol::is_room(&a.room));
    }

    #[test]
    fn keys_are_case_sensitive_and_separate() {
        let a = Keys::derive("Correct horse").unwrap();
        let b = Keys::derive("correct horse").unwrap();
        assert_ne!(a.room, b.room);
        assert_ne!(a.handshake(), b.handshake());
        let (c, h) = ([1u8; NONCE_LEN], [2u8; NONCE_LEN]);
        assert_ne!(a.session(&c, &h), a.session(&h, &c));
        assert_ne!(a.session(&c, &h), a.media(&c));
        assert_ne!(&a.handshake()[..], a.room.as_bytes());
    }

    #[test]
    fn short_and_long_keys_are_refused() {
        assert_eq!(normalize("short").unwrap_err(), KeyProblem::TooShort);
        assert_eq!(
            normalize(&"x".repeat(129)).unwrap_err(),
            KeyProblem::TooLong
        );
        assert_eq!(normalize(" 12345678 ").unwrap(), "12345678");
    }

    #[test]
    fn generated_keys_are_valid_and_differ() {
        let a = generate_key();
        let b = generate_key();
        assert_ne!(a, b);
        assert_eq!(a.len(), 19);
        assert_eq!(a.matches('-').count(), 3);
        assert!(normalize(&a).is_ok());
        assert!(a.bytes().all(|c| c == b'-' || KEY_ALPHABET.contains(&c)));
    }
}
