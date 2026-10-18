//! Backtalk remote access, shared by the Android and iPhone apps through
//! UniFFI.
//!
//! * [`host::HostSession`]: share this phone. Backtalk on the phone gets
//!   the controllers' touches, which it treats as its own, and hands over
//!   the audio it plays.
//! * [`controller::ControllerSession`]: control a phone, and hear its
//!   Backtalk.
//!
//! Both meet through a relay server ([`link`]); everything between them is
//! sealed with keys made from their shared access key ([`keys`],
//! [`seal`]), and audio travels with AudioNet's transport
//! ([`host_audio`], [`audio_out`]). See `docs/remote-access.md`.

pub mod audio_out;
pub mod controller;
pub mod host;
pub mod host_audio;
pub mod keys;
pub mod link;
pub mod peer;
pub mod seal;

#[cfg(target_os = "android")]
mod android;

use std::sync::{LazyLock, Once};
use std::time::Duration;

uniffi::setup_scaffolding!();

/// Without anything from the relay for this long (it pings every 20
/// seconds), the connection is taken as lost.
pub(crate) const RELAY_SILENCE: Duration = Duration::from_secs(60);

static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("backtalk-remote")
        .enable_all()
        .build()
        .expect("the remote access runtime starts")
});

pub(crate) fn runtime() -> &'static tokio::runtime::Runtime {
    &RUNTIME
}

/// Makes ring rustls's cryptography for the relay's TLS, once.
pub(crate) fn install_crypto_provider() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// How long to wait before connecting again: 1, 2, 4, 8, 15, then 30
/// seconds, with some randomness so that many devices do not come back at
/// once.
#[derive(Debug, Default)]
pub(crate) struct Backoff {
    attempts: u32,
}

impl Backoff {
    pub(crate) fn delay(&mut self) -> Duration {
        const STEPS_MS: [u64; 6] = [1_000, 2_000, 4_000, 8_000, 15_000, 30_000];
        let base = STEPS_MS[(self.attempts as usize).min(STEPS_MS.len() - 1)];
        self.attempts = self.attempts.saturating_add(1);
        let jitter = rand::random::<u64>() % (base / 4 + 1);
        Duration::from_millis(base + jitter)
    }

    pub(crate) fn reset(&mut self) {
        self.attempts = 0;
    }
}

/// A new random access key, such as `kq7m-2xpr-9tdf-h4wa`.
#[uniffi::export]
pub fn generate_access_key() -> String {
    keys::generate_key()
}

/// What is wrong with `key` as an access key, in words, or `None` if it
/// can be used.
#[uniffi::export]
pub fn access_key_problem(key: String) -> Option<String> {
    keys::normalize(&key).err().map(|p| p.to_string())
}

/// What is wrong with `server` as a relay server setting, in words, or
/// `None` if it can be used.
#[uniffi::export]
pub fn relay_server_problem(server: String) -> Option<String> {
    link::Server::parse(&server).err()
}

/// The version of this library.
#[uniffi::export]
pub fn core_version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}

#[cfg(test)]
mod tests;
