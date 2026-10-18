//! The connection to the relay: the WebSocket, joining a room, and the UDP
//! socket for audio.

use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::Duration;

use backtalk_remote_protocol::{
    PROTOCOL_VERSION, Peer, RelayEvent, RelayRequest, Role, UDP_HELLO_MAGIC, UDP_TOKEN_LEN,
    WEBSOCKET_PATH, from_hex,
};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

pub type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Where the relay is, from what the user typed in the relay server
/// setting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    /// The WebSocket URL.
    pub url: String,
    /// The host name, for UDP.
    pub host: String,
}

impl Server {
    /// Reads the setting: a host name (`mad-gamer.com`), a host and port
    /// (`relay.example.org:8443`), both over TLS, or a whole `wss://` or
    /// `ws://` URL (`ws://` only for testing on a local network). A URL
    /// without a path gets the relay's usual path.
    pub fn parse(setting: &str) -> Result<Server, String> {
        let s = setting.trim();
        if s.is_empty() {
            return Err("Enter a relay server.".into());
        }
        let (scheme, rest) = if let Some(r) = s.strip_prefix("wss://") {
            ("wss", r)
        } else if let Some(r) = s.strip_prefix("ws://") {
            ("ws", r)
        } else if let Some(r) = s.strip_prefix("https://") {
            ("wss", r)
        } else if let Some(r) = s.strip_prefix("http://") {
            ("ws", r)
        } else if s.contains("://") {
            return Err("The relay server must be a host name, or a wss:// address.".into());
        } else {
            ("wss", s)
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        let path = if path.is_empty() || path == "/" {
            WEBSOCKET_PATH
        } else {
            path
        };
        let host = if let Some(bracketed) = authority.strip_prefix('[') {
            // An IPv6 address: [::1]:port
            bracketed.split(']').next().unwrap_or_default()
        } else {
            authority.split(':').next().unwrap_or_default()
        };
        if host.is_empty() || authority.chars().any(|c| c.is_whitespace() || c == '@') {
            return Err("The relay server's name is not valid.".into());
        }
        Ok(Server {
            url: format!("{scheme}://{authority}{path}"),
            host: host.to_owned(),
        })
    }
}

/// What the relay said when the app joined.
#[derive(Clone, Debug)]
pub struct Joined {
    pub peer_id: u32,
    pub udp_port: u16,
    pub udp_token: [u8; UDP_TOKEN_LEN],
    pub peers: Vec<Peer>,
}

/// Connects to the relay and joins `room`.
pub async fn join(server: &Server, room: &str, role: Role) -> Result<(Socket, Joined), String> {
    crate::install_crypto_provider();
    let (mut socket, _) = tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio_tungstenite::connect_async(server.url.as_str()),
    )
    .await
    .map_err(|_| "The relay server did not answer in time.".to_owned())?
    .map_err(|e| format!("Could not reach the relay server: {e}"))?;
    send(
        &mut socket,
        &RelayRequest::Join {
            version: PROTOCOL_VERSION,
            room: room.to_owned(),
            role,
        },
    )
    .await?;
    let joined = tokio::time::timeout(CONNECT_TIMEOUT, async {
        loop {
            let event = match next(&mut socket).await? {
                Some(Incoming::Event(e)) => Some(e),
                Some(Incoming::Alive) => continue,
                None => None,
            };
            match event {
                Some(RelayEvent::Joined {
                    peer_id,
                    udp_port,
                    udp_token,
                    peers,
                    ..
                }) => {
                    let udp_token = from_hex::<UDP_TOKEN_LEN>(&udp_token)
                        .ok_or_else(|| "The relay server sent a bad token.".to_owned())?;
                    return Ok(Joined {
                        peer_id,
                        udp_port,
                        udp_token,
                        peers,
                    });
                }
                Some(RelayEvent::Error { message, .. }) => return Err(message),
                Some(_) => {}
                None => return Err("The relay server closed the connection.".to_owned()),
            }
        }
    })
    .await
    .map_err(|_| "The relay server did not let this device in, in time.".to_owned())??;
    Ok((socket, joined))
}

pub async fn send(socket: &mut Socket, request: &RelayRequest) -> Result<(), String> {
    let text = serde_json::to_string(request).expect("requests serialize");
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(|e| format!("Lost the connection to the relay server: {e}"))
}

/// What came from the relay.
#[derive(Debug)]
pub enum Incoming {
    Event(RelayEvent),
    /// A ping or another frame that is not an event: the connection is
    /// alive. (tungstenite answers pings itself.)
    Alive,
}

/// The next thing from the relay; `Ok(None)` when the connection closed.
pub async fn next(socket: &mut Socket) -> Result<Option<Incoming>, String> {
    match socket.next().await {
        None | Some(Ok(Message::Close(_))) => Ok(None),
        Some(Err(e)) => Err(format!("Lost the connection to the relay server: {e}")),
        // An event this version does not know only shows the connection
        // is alive.
        Some(Ok(Message::Text(text))) => Ok(Some(
            serde_json::from_str(&text).map_or(Incoming::Alive, Incoming::Event),
        )),
        Some(Ok(_)) => Ok(Some(Incoming::Alive)),
    }
}

/// A UDP socket for audio, with the relay's address and the hello that
/// tells the relay where the socket is.
#[derive(Debug)]
pub struct Udp {
    pub socket: UdpSocket,
    pub relay: SocketAddr,
    hello: Vec<u8>,
}

impl Udp {
    /// Opens a socket toward the relay. Resolves the relay's name: call it
    /// off the async threads (it may wait on DNS).
    pub fn open(server: &Server, joined: &Joined) -> Result<Udp, String> {
        let relay = (server.host.as_str(), joined.udp_port)
            .to_socket_addrs()
            .map_err(|e| format!("Could not find the relay server's audio address: {e}"))?
            .next()
            .ok_or_else(|| "The relay server has no audio address.".to_owned())?;
        let bind: SocketAddr = if relay.is_ipv4() {
            "0.0.0.0:0".parse().expect("valid")
        } else {
            "[::]:0".parse().expect("valid")
        };
        let socket = audionet_engine::runtime::bind_media_socket(bind)
            .map_err(|e| format!("Could not open a socket for audio: {e}"))?;
        let mut hello = UDP_HELLO_MAGIC.to_vec();
        hello.extend_from_slice(&joined.udp_token);
        Ok(Udp {
            socket,
            relay,
            hello,
        })
    }

    /// A second handle to the same socket, for the hellos once AudioNet's
    /// runtime owns the first.
    pub fn hello_sender(&self) -> Result<HelloSender, String> {
        Ok(HelloSender {
            socket: self
                .socket
                .try_clone()
                .map_err(|e| format!("Could not share the audio socket: {e}"))?,
            relay: self.relay,
            hello: self.hello.clone(),
        })
    }
}

/// Sends the UDP hello, again every few seconds, so the relay and any NAT
/// in between keep the socket's address.
#[derive(Debug)]
pub struct HelloSender {
    socket: UdpSocket,
    relay: SocketAddr,
    hello: Vec<u8>,
}

impl HelloSender {
    pub fn send(&self) {
        // Non-blocking once AudioNet's runtime set the socket so; a
        // failure is retried at the next interval.
        let _ = self.socket.send_to(&self.hello, self.relay);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_names_become_tls_urls() {
        let s = Server::parse(" mad-gamer.com ").unwrap();
        assert_eq!(s.url, "wss://mad-gamer.com/backtalk-remote/ws");
        assert_eq!(s.host, "mad-gamer.com");
        let s = Server::parse("relay.example.org:8443").unwrap();
        assert_eq!(s.url, "wss://relay.example.org:8443/backtalk-remote/ws");
        assert_eq!(s.host, "relay.example.org");
    }

    #[test]
    fn urls_keep_their_scheme_and_path() {
        let s = Server::parse("ws://192.168.1.5:39860").unwrap();
        assert_eq!(s.url, "ws://192.168.1.5:39860/backtalk-remote/ws");
        assert_eq!(s.host, "192.168.1.5");
        let s = Server::parse("https://example.org/custom/ws").unwrap();
        assert_eq!(s.url, "wss://example.org/custom/ws");
        let s = Server::parse("ws://[::1]:39860/").unwrap();
        assert_eq!(s.host, "::1");
        assert_eq!(s.url, "ws://[::1]:39860/backtalk-remote/ws");
    }

    #[test]
    fn bad_settings_are_refused() {
        assert!(Server::parse("").is_err());
        assert!(Server::parse("ftp://example.org").is_err());
        assert!(Server::parse("wss://").is_err());
        assert!(Server::parse("user@example.org").is_err());
        assert!(Server::parse("exa mple.org").is_err());
    }
}
