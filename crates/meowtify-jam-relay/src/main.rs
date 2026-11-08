//! `meowtify-jam-relay [ADDRESS]`: runs the jam relay, with a TURN server built in.
//!
//! It needs no setup. It finds its own public address, starts TURN, and prints the address to
//! put in Meowtify. Options:
//!
//! - `ADDRESS` is where the relay listens; the default is `0.0.0.0:8787`.
//! - `--public-ip IP` (or `MEOWTIFY_PUBLIC_IP`) names the public address when it cannot be found.
//! - `--no-turn` runs the relay without the TURN server.

use std::net::{IpAddr, SocketAddr};

use meowtify_jam_relay::turn_server::{self, Turn};

#[tokio::main]
async fn main() {
    let mut addr = "0.0.0.0:8787".to_string();
    let mut public: Option<String> = std::env::var("MEOWTIFY_PUBLIC_IP").ok().filter(|s| !s.is_empty());
    let mut want_turn = true;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--public-ip" => public = args.next(),
            "--no-turn" => want_turn = false,
            _ => addr = arg,
        }
    }
    let addr: SocketAddr = match addr.parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("usage: meowtify-jam-relay [ADDRESS:PORT] [--public-ip IP] [--no-turn] ({e})");
            std::process::exit(2);
        }
    };
    let listener = match meowtify_jam_relay::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot listen on {addr}: {e}");
            std::process::exit(1);
        }
    };

    let public_ip: Option<IpAddr> = match public {
        Some(text) => match text.parse() {
            Ok(ip) => Some(ip),
            Err(_) => {
                eprintln!("--public-ip is not an IP address: {text}");
                std::process::exit(2);
            }
        },
        None => turn_server::public_ip().await,
    };

    let mut opts = meowtify_jam_relay::Options::default();
    match (want_turn, public_ip) {
        (false, _) => eprintln!("TURN is off, so players cannot hide their IP address."),
        (true, None) => eprintln!("Could not find this machine's public address, so TURN is off. Start again with --public-ip <address>."),
        (true, Some(ip)) => match Turn::start(ip, turn_server::DEFAULT_PORT).await {
            Ok(turn) => {
                eprintln!("TURN is on at {ip}, UDP port {}. Players can hide their IP address.", turn.port());
                opts.turn = Some(turn);
            }
            Err(e) => eprintln!("TURN is off: {e}"),
        },
    }

    eprintln!("jam relay listening on {addr}");
    let host = public_ip.map_or_else(|| addr.ip().to_string(), |ip| ip.to_string());
    eprintln!("Relay address for Meowtify: ws://{host}:{}", addr.port());
    if opts.turn.is_some() {
        eprintln!("If a firewall is on, allow TCP {} and UDP {} and {}-{}.", addr.port(), turn_server::DEFAULT_PORT, turn_server::RELAY_PORTS.start(), turn_server::RELAY_PORTS.end());
    }
    meowtify_jam_relay::serve(listener, opts).await;
}
