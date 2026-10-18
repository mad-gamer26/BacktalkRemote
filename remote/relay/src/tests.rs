//! The relay over real sockets on this machine.

use super::*;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message as WsMessage;

type Client =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn start() -> (String, SocketAddr) {
    let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let udp_addr = udp.local_addr().unwrap();
    let relay = Relay::new(udp, udp_addr.port(), Limits::default());
    tokio::spawn(relay.clone().run_udp());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = relay
        .router()
        .into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (
        format!("ws://{addr}{}", backtalk_remote_protocol::WEBSOCKET_PATH),
        udp_addr,
    )
}

async fn send(c: &mut Client, r: &RelayRequest) {
    c.send(WsMessage::Text(serde_json::to_string(r).unwrap().into()))
        .await
        .unwrap();
}

async fn next(c: &mut Client) -> RelayEvent {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), c.next())
            .await
            .expect("an event in time")
            .unwrap()
            .unwrap();
        if let WsMessage::Text(t) = m {
            return serde_json::from_str(&t).unwrap();
        }
    }
}

async fn join(url: &str, room: &str, role: Role) -> (Client, u32, u16, String, Vec<Peer>) {
    let (mut c, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    send(
        &mut c,
        &RelayRequest::Join {
            version: PROTOCOL_VERSION,
            room: room.into(),
            role,
        },
    )
    .await;
    match next(&mut c).await {
        RelayEvent::Joined {
            peer_id,
            udp_port,
            udp_token,
            peers,
            ..
        } => (c, peer_id, udp_port, udp_token, peers),
        other => panic!("expected joined, got {other:?}"),
    }
}

fn hello(token: &str) -> Vec<u8> {
    let mut d = UDP_HELLO_MAGIC.to_vec();
    d.extend_from_slice(&backtalk_remote_protocol::from_hex::<UDP_TOKEN_LEN>(token).unwrap());
    d
}

#[tokio::test]
async fn host_and_controller_exchange_messages_and_media() {
    let (url, udp_addr) = start().await;
    let room = "a1".repeat(32);
    let (mut host, host_id, port, host_token, peers) = join(&url, &room, Role::Host).await;
    assert!(peers.is_empty());
    assert_eq!(port, udp_addr.port());
    let (mut ctrl, ctrl_id, _, ctrl_token, peers) = join(&url, &room, Role::Controller).await;
    assert_eq!(
        peers,
        vec![Peer {
            peer_id: host_id,
            role: Role::Host
        }]
    );
    assert_eq!(
        next(&mut host).await,
        RelayEvent::PeerJoined {
            peer: Peer {
                peer_id: ctrl_id,
                role: Role::Controller
            }
        }
    );

    send(
        &mut ctrl,
        &RelayRequest::Send {
            to: host_id,
            data: "c2h".into(),
        },
    )
    .await;
    assert_eq!(
        next(&mut host).await,
        RelayEvent::Message {
            from: ctrl_id,
            data: "c2h".into()
        }
    );
    send(
        &mut host,
        &RelayRequest::Send {
            to: ctrl_id,
            data: "h2c".into(),
        },
    )
    .await;
    assert_eq!(
        next(&mut ctrl).await,
        RelayEvent::Message {
            from: host_id,
            data: "h2c".into()
        }
    );

    let host_udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let ctrl_udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    host_udp
        .send_to(&hello(&host_token), udp_addr)
        .await
        .unwrap();
    ctrl_udp
        .send_to(&hello(&ctrl_token), udp_addr)
        .await
        .unwrap();
    assert_eq!(next(&mut host).await, RelayEvent::UdpReady);
    assert_eq!(next(&mut ctrl).await, RelayEvent::UdpReady);

    host_udp.send_to(b"media packet", udp_addr).await.unwrap();
    let mut buf = [0u8; 64];
    let (n, from) = tokio::time::timeout(Duration::from_secs(5), ctrl_udp.recv_from(&mut buf))
        .await
        .expect("media in time")
        .unwrap();
    assert_eq!(&buf[..n], b"media packet");
    assert_eq!(from, udp_addr);

    // A controller's datagrams go nowhere.
    ctrl_udp
        .send_to(b"from controller", udp_addr)
        .await
        .unwrap();
    let nothing =
        tokio::time::timeout(Duration::from_millis(300), host_udp.recv_from(&mut buf)).await;
    assert!(
        nothing.is_err(),
        "the host received a controller's datagram"
    );

    drop(ctrl);
    assert_eq!(
        next(&mut host).await,
        RelayEvent::PeerLeft { peer_id: ctrl_id }
    );
}

#[tokio::test]
async fn controllers_cannot_message_each_other() {
    let (url, _) = start().await;
    let room = "b2".repeat(32);
    let (_host, _, _, _, _) = join(&url, &room, Role::Host).await;
    let (mut first, _, _, _, _) = join(&url, &room, Role::Controller).await;
    let (_second, second_id, _, _, _) = join(&url, &room, Role::Controller).await;
    // The first controller hears of the second, then is refused.
    assert!(matches!(
        next(&mut first).await,
        RelayEvent::PeerJoined { .. }
    ));
    send(
        &mut first,
        &RelayRequest::Send {
            to: second_id,
            data: "x".into(),
        },
    )
    .await;
    assert!(matches!(
        next(&mut first).await,
        RelayEvent::Error {
            code: ErrorCode::NoPeer,
            fatal: false,
            ..
        }
    ));
}

#[tokio::test]
async fn a_new_host_replaces_the_old() {
    let (url, _) = start().await;
    let room = "c3".repeat(32);
    let (mut old, old_id, _, _, _) = join(&url, &room, Role::Host).await;
    let (mut ctrl, _, _, _, _) = join(&url, &room, Role::Controller).await;
    assert!(matches!(
        next(&mut old).await,
        RelayEvent::PeerJoined { .. }
    ));
    let (_new, new_id, _, _, peers) = join(&url, &room, Role::Host).await;
    assert!(peers.iter().all(|p| p.peer_id != old_id));
    assert!(matches!(
        next(&mut old).await,
        RelayEvent::Error {
            code: ErrorCode::Replaced,
            fatal: true,
            ..
        }
    ));
    assert_eq!(
        next(&mut ctrl).await,
        RelayEvent::PeerLeft { peer_id: old_id }
    );
    assert_eq!(
        next(&mut ctrl).await,
        RelayEvent::PeerJoined {
            peer: Peer {
                peer_id: new_id,
                role: Role::Host
            }
        }
    );
}

#[tokio::test]
async fn bad_joins_are_refused() {
    let (url, _) = start().await;
    let (mut c, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    send(
        &mut c,
        &RelayRequest::Join {
            version: PROTOCOL_VERSION + 1,
            room: "d4".repeat(32),
            role: Role::Host,
        },
    )
    .await;
    assert!(matches!(
        next(&mut c).await,
        RelayEvent::Error {
            code: ErrorCode::Version,
            ..
        }
    ));
    let (mut c, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    send(
        &mut c,
        &RelayRequest::Join {
            version: PROTOCOL_VERSION,
            room: "not a room".into(),
            role: Role::Host,
        },
    )
    .await;
    assert!(matches!(
        next(&mut c).await,
        RelayEvent::Error {
            code: ErrorCode::BadRequest,
            ..
        }
    ));
}
