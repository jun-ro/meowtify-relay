//! Checks a relay that is already running somewhere: `MEOWTIFY_RELAY_HTTP=https://host` and
//! `cargo test -p meowtify-jam-relay --test remote -- --ignored`. Two clients each get a relayed
//! address from its TURN server and pass a packet to each other through it, which is what two
//! players with hidden IP addresses do.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::UdpSocket;
use turn::client::{Client, ClientConfig};
use webrtc_util::Conn;

async fn allocate(server: &serde_json::Value) -> (Client, impl Conn) {
    let urls = server["urls"].as_array().unwrap();
    let addr = urls.iter().filter_map(|u| u.as_str()).find_map(|u| u.strip_prefix("turn:")).unwrap().split('?').next().unwrap().to_string();
    let client = Client::new(ClientConfig {
        stun_serv_addr: addr.clone(),
        turn_serv_addr: addr,
        username: server["username"].as_str().unwrap().into(),
        password: server["credential"].as_str().unwrap().into(),
        realm: String::new(),
        software: String::new(),
        rto_in_ms: 0,
        conn: Arc::new(UdpSocket::bind("0.0.0.0:0").await.unwrap()),
        vnet: None,
    })
    .await
    .unwrap();
    client.listen().await.unwrap();
    let relayed = client.allocate().await.expect("TURN accepts the credentials");
    (client, relayed)
}

#[tokio::test]
#[ignore = "needs a running relay"]
async fn two_clients_pass_a_packet_through_the_remote_turn_server() {
    let base = std::env::var("MEOWTIFY_RELAY_HTTP").expect("set MEOWTIFY_RELAY_HTTP to the relay's https address");
    let out = std::process::Command::new("curl").args(["--silent", "--fail", "--max-time", "10", &format!("{base}/turn")]).output().unwrap();
    assert!(out.status.success(), "GET /turn failed");
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let server = &json["iceServers"][0];

    let (_a, a) = allocate(server).await;
    let (_b, b) = allocate(server).await;
    let (a_addr, b_addr) = (a.local_addr().unwrap(), b.local_addr().unwrap());
    println!("relayed addresses: {a_addr} and {b_addr}");

    // Each side names the other first, which opens a permission for it.
    a.send_to(b"open", b_addr).await.unwrap();
    b.send_to(b"open", a_addr).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    a.send_to(b"hello", b_addr).await.unwrap();
    let mut buf = [0u8; 64];
    let got = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (n, _) = b.recv_from(&mut buf).await.unwrap();
            if &buf[..n] == b"hello" {
                return;
            }
        }
    })
    .await;
    assert!(got.is_ok(), "the packet did not arrive: are UDP ports 3478 and 49152-49351 open?");
}
