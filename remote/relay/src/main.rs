//! `backtalk-remote-relay`: runs the relay.
//!
//! ```text
//! backtalk-remote-relay [--listen 127.0.0.1:39860] [--udp 0.0.0.0:39861] [--udp-public-port PORT]
//! ```
//!
//! `--listen` is the plain HTTP address for the WebSocket; put a TLS proxy
//! (nginx) in front of it, as `deploy/` shows. `--udp` is where media
//! comes in and goes out; it must be reachable from the internet.
//! `--udp-public-port` is the port apps are told to use, when a firewall
//! maps a different one.

#![forbid(unsafe_code)]

use std::net::SocketAddr;

use backtalk_remote_relay::{Limits, Relay};

struct Options {
    listen: SocketAddr,
    udp: SocketAddr,
    udp_public_port: Option<u16>,
}

fn usage() -> ! {
    eprintln!(
        "Usage: backtalk-remote-relay [--listen ADDRESS:PORT] [--udp ADDRESS:PORT] [--udp-public-port PORT]"
    );
    std::process::exit(2);
}

fn options() -> Options {
    let mut o = Options {
        listen: "127.0.0.1:39860".parse().expect("valid address"),
        udp: "0.0.0.0:39861".parse().expect("valid address"),
        udp_public_port: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next().unwrap_or_else(|| usage());
        match arg.as_str() {
            "--listen" => o.listen = value.parse().unwrap_or_else(|_| usage()),
            "--udp" => o.udp = value.parse().unwrap_or_else(|_| usage()),
            "--udp-public-port" => {
                o.udp_public_port = Some(value.parse().unwrap_or_else(|_| usage()))
            }
            _ => usage(),
        }
    }
    o
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "backtalk_remote_relay=info".into()),
        )
        .with_ansi(false)
        .init();
    let o = options();
    let udp = match tokio::net::UdpSocket::bind(o.udp).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Media socket: cannot listen on {}: {e}", o.udp);
            std::process::exit(1);
        }
    };
    let relay = Relay::new(
        udp,
        o.udp_public_port.unwrap_or(o.udp.port()),
        Limits::default(),
    );
    tokio::spawn(relay.clone().run_udp());
    let listener = match tokio::net::TcpListener::bind(o.listen).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Web socket: cannot listen on {}: {e}", o.listen);
            std::process::exit(1);
        }
    };
    tracing::info!(
        "listening on {} (WebSocket) and {} (media)",
        o.listen,
        o.udp
    );
    let app = relay
        .router()
        .into_make_service_with_connect_info::<SocketAddr>();
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
    {
        eprintln!("Web socket: the server stopped: {e}");
        std::process::exit(1);
    }
}
