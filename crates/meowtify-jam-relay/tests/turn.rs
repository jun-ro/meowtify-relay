//! The built-in TURN server: `GET /turn` hands out credentials, and a TURN client can use them.

use std::sync::Arc;

use meowtify_jam_relay::turn_server::Turn;
use meowtify_jam_relay::{bind, serve, Options};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use turn::client::{Client, ClientConfig};
use webrtc_util::Conn;

async fn get_turn(addr: std::net::SocketAddr) -> (String, String) {
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(b"GET /turn HTTP/1.1\r\nHost: x\r\n\r\n").await.unwrap();
    let mut text = String::new();
    s.read_to_string(&mut text).await.unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    (head.lines().next().unwrap().to_string(), body.to_string())
}

#[tokio::test]
async fn a_relay_without_turn_says_so() {
    let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener, Options::default()));
    let (status, _) = get_turn(addr).await;
    assert!(status.contains("404"), "{status}");
}

#[tokio::test]
async fn the_credentials_it_hands_out_open_a_relay_allocation() {
    let turn = Turn::start("127.0.0.1".parse().unwrap(), 34780).await.unwrap();
    let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener, Options { turn: Some(turn), ..Options::default() }));

    let (status, body) = get_turn(addr).await;
    assert!(status.contains("200"), "{status}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let server = &json["iceServers"][0];
    let urls = server["urls"].as_array().unwrap();
    assert!(urls.iter().any(|u| u == "turn:127.0.0.1:34780?transport=udp"), "{urls:?}");

    let conn = Arc::new(UdpSocket::bind("0.0.0.0:0").await.unwrap());
    let client = Client::new(ClientConfig {
        stun_serv_addr: "127.0.0.1:34780".into(),
        turn_serv_addr: "127.0.0.1:34780".into(),
        username: server["username"].as_str().unwrap().into(),
        password: server["credential"].as_str().unwrap().into(),
        realm: String::new(),
        software: String::new(),
        rto_in_ms: 0,
        conn,
        vnet: None,
    })
    .await
    .unwrap();
    client.listen().await.unwrap();
    let relayed = client.allocate().await.expect("the server accepts its own credentials");
    assert!(relayed.local_addr().unwrap().port() >= 49152);
    client.close().await.unwrap();

    // The wrong password gets nowhere.
    let conn = Arc::new(UdpSocket::bind("0.0.0.0:0").await.unwrap());
    let bad = Client::new(ClientConfig {
        stun_serv_addr: "127.0.0.1:34780".into(),
        turn_serv_addr: "127.0.0.1:34780".into(),
        username: server["username"].as_str().unwrap().into(),
        password: "wrong".into(),
        realm: String::new(),
        software: String::new(),
        rto_in_ms: 0,
        conn,
        vnet: None,
    })
    .await
    .unwrap();
    bad.listen().await.unwrap();
    assert!(bad.allocate().await.is_err());
}
