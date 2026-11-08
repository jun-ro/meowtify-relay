//! A built-in TURN server, so players who choose to hide their IP address have a relay to use
//! without the owner of this relay installing anything.
//!
//! Credentials are made up on the fly (the "TURN REST" scheme): a user name that holds the time
//! it expires, and a password that is a keyed hash of that user name. The key is made at start
//! and never leaves this process, so there is nothing to configure and nothing to leak.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;
use tokio::net::UdpSocket;
use turn::auth::{generate_auth_key, AuthHandler};
use turn::relay::relay_range::RelayAddressGeneratorRanges;
use turn::server::config::{ConnConfig, ServerConfig};
use turn::server::Server;
use turn::Error as TurnError;
use webrtc_util::vnet::net::Net;

const REALM: &str = "meowtify";
/// How long a credential works. A jam is not longer than a day.
const LIFETIME: u64 = 24 * 60 * 60;
/// UDP ports the TURN server hands out for relayed traffic. Two hundred is plenty for a jam.
pub const RELAY_PORTS: std::ops::RangeInclusive<u16> = 49152..=49351;
/// The standard TURN port.
pub const DEFAULT_PORT: u16 = 3478;

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn password(secret: &[u8], username: &str) -> String {
    let mut mac = <Hmac<Sha1> as KeyInit>::new_from_slice(secret).expect("HMAC takes a key of any size");
    mac.update(username.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

struct Auth {
    secret: [u8; 32],
}

impl AuthHandler for Auth {
    fn auth_handle(&self, username: &str, realm: &str, _src: SocketAddr) -> Result<Vec<u8>, TurnError> {
        let expires: u64 = username.split(':').next().and_then(|t| t.parse().ok()).ok_or(TurnError::ErrFakeErr)?;
        if expires < now() {
            return Err(TurnError::ErrFakeErr);
        }
        Ok(generate_auth_key(username, realm, &password(&self.secret, username)))
    }
}

/// The running TURN server and what is needed to hand out credentials for it.
pub struct Turn {
    secret: [u8; 32],
    public: IpAddr,
    port: u16,
    _server: Server,
}

impl Turn {
    /// Starts TURN on UDP `port` of every interface. `public` is the address players reach this
    /// machine at; relayed traffic is announced with it.
    pub async fn start(public: IpAddr, port: u16) -> Result<Arc<Turn>, String> {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).map_err(|e| format!("no random numbers: {e}"))?;
        let socket = UdpSocket::bind(("0.0.0.0", port)).await.map_err(|e| format!("cannot bind UDP port {port}: {e}"))?;
        let server = Server::new(ServerConfig {
            conn_configs: vec![ConnConfig {
                conn: Arc::new(socket),
                relay_addr_generator: Box::new(RelayAddressGeneratorRanges {
                    relay_address: public,
                    min_port: *RELAY_PORTS.start(),
                    max_port: *RELAY_PORTS.end(),
                    max_retries: 10,
                    address: "0.0.0.0".to_owned(),
                    net: Arc::new(Net::new(None)),
                }),
            }],
            realm: REALM.to_owned(),
            auth_handler: Arc::new(Auth { secret }),
            channel_bind_timeout: Duration::from_secs(0),
            alloc_close_notify: None,
        })
        .await
        .map_err(|e| e.to_string())?;
        Ok(Arc::new(Turn { secret, public, port, _server: server }))
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The answer to `GET /turn`: the `iceServers` list a WebRTC client takes as it is.
    pub fn ice_servers(&self) -> String {
        let username = format!("{}:meowtify", now() + LIFETIME);
        let credential = password(&self.secret, &username);
        let host = match self.public {
            IpAddr::V4(a) => a.to_string(),
            IpAddr::V6(a) => format!("[{a}]"),
        };
        format!(
            r#"{{"iceServers":[{{"urls":["stun:{host}:{p}","turn:{host}:{p}?transport=udp"],"username":"{username}","credential":"{credential}"}}]}}"#,
            p = self.port
        )
    }
}

/// Finds this machine's public IPv4 address by asking a STUN server, which answers with the
/// address it saw the request come from. `None` when there is no answer.
pub async fn public_ip() -> Option<IpAddr> {
    let server = tokio::net::lookup_host("stun.cloudflare.com:3478").await.ok()?.find(|a| a.is_ipv4())?;
    let socket = UdpSocket::bind("0.0.0.0:0").await.ok()?;
    let mut id = [0u8; 12];
    getrandom::fill(&mut id).ok()?;
    let mut request = vec![0, 1, 0, 0, 0x21, 0x12, 0xa4, 0x42];
    request.extend_from_slice(&id);
    socket.send_to(&request, server).await.ok()?;
    let mut buf = [0u8; 512];
    let (n, _) = tokio::time::timeout(Duration::from_secs(3), socket.recv_from(&mut buf)).await.ok()?.ok()?;
    parse_mapped(&buf[..n], &id)
}

/// The address in a STUN binding answer to the request with transaction `id`.
fn parse_mapped(msg: &[u8], id: &[u8; 12]) -> Option<IpAddr> {
    if msg.len() < 20 || msg[0] != 0x01 || msg[1] != 0x01 || &msg[8..20] != id {
        return None;
    }
    let end = (20 + usize::from(u16::from_be_bytes([msg[2], msg[3]]))).min(msg.len());
    let mut at = 20;
    while at + 4 <= end {
        let (kind, len) = (u16::from_be_bytes([msg[at], msg[at + 1]]), usize::from(u16::from_be_bytes([msg[at + 2], msg[at + 3]])));
        let value = msg.get(at + 4..at + 4 + len)?;
        if (kind == 0x0020 || kind == 0x0001) && len >= 8 && value[1] == 1 {
            let mut ip = [value[4], value[5], value[6], value[7]];
            if kind == 0x0020 {
                for (b, m) in ip.iter_mut().zip([0x21, 0x12, 0xa4, 0x42]) {
                    *b ^= m;
                }
            }
            return Some(IpAddr::from(ip));
        }
        at += 4 + len.div_ceil(4) * 4;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_address_a_stun_server_saw() {
        let id = [7u8; 12];
        let mut msg = vec![0x01, 0x01, 0, 12, 0x21, 0x12, 0xa4, 0x42];
        msg.extend_from_slice(&id);
        // XOR-MAPPED-ADDRESS for 203.0.113.5 port 4000
        msg.extend_from_slice(&[0, 0x20, 0, 8, 0, 1, 0x21 ^ 0x0f, 0x12 ^ 0xa0]);
        msg.extend_from_slice(&[203 ^ 0x21, 0x12, 113 ^ 0xa4, 5 ^ 0x42]);
        assert_eq!(parse_mapped(&msg, &id), Some(IpAddr::from([203, 0, 113, 5])));
        assert_eq!(parse_mapped(&msg, &[0; 12]), None);
        assert_eq!(parse_mapped(&msg[..10], &id), None);
    }

    #[test]
    fn credentials_expire_and_need_the_key() {
        let auth = Auth { secret: [1; 32] };
        let live = format!("{}:x", now() + 60);
        let dead = format!("{}:x", now() - 60);
        let src: SocketAddr = "127.0.0.1:1".parse().unwrap();
        assert_eq!(auth.auth_handle(&live, REALM, src).unwrap(), generate_auth_key(&live, REALM, &password(&[1; 32], &live)));
        assert!(auth.auth_handle(&dead, REALM, src).is_err());
        assert!(auth.auth_handle("nonsense", REALM, src).is_err());
        assert_ne!(password(&[1; 32], &live), password(&[2; 32], &live));
    }
}
