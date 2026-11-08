//! A host and a guest meet through a real relay on a local port.

use futures_util::{SinkExt, StreamExt};
use meowtify_jam::{ClientMsg, ErrorCode, ServerMsg, Target};
use serde_json::json;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

const KEY: &str = "0123456789abcdef0123";

async fn start() -> std::net::SocketAddr {
    let listener = meowtify_jam_relay::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(meowtify_jam_relay::serve(listener, Default::default()));
    addr
}

async fn connect(addr: std::net::SocketAddr, code: &str) -> Ws {
    tokio_tungstenite::connect_async(format!("ws://{addr}/jam/{code}")).await.unwrap().0
}

async fn say(ws: &mut Ws, msg: &ClientMsg) {
    ws.send(Message::text(serde_json::to_string(msg).unwrap())).await.unwrap();
}

async fn hear(ws: &mut Ws) -> ServerMsg {
    loop {
        let next = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next()).await.expect("a message in time");
        if let Message::Text(t) = next.expect("not closed").expect("no error") {
            return serde_json::from_str(t.as_str()).unwrap();
        }
    }
}

fn hello(name: &str, key: Option<&str>) -> ClientMsg {
    ClientMsg::Hello { name: name.into(), host_key: key.map(Into::into), resume: None, hue: None }
}

#[tokio::test]
async fn a_host_and_a_guest_meet_and_talk() {
    let addr = start().await;
    let mut host = connect(addr, "room42").await;
    say(&mut host, &hello("Hana", Some(KEY))).await;
    assert!(matches!(hear(&mut host).await, ServerMsg::Welcome { host: true, .. }));

    let mut guest = connect(addr, "ROOM42").await; // codes ignore case
    say(&mut guest, &hello("Ann", None)).await;
    assert!(matches!(hear(&mut guest).await, ServerMsg::Welcome { you: 1, .. }));
    assert!(matches!(hear(&mut host).await, ServerMsg::Joined { .. }));

    say(&mut guest, &ClientMsg::Send { to: Target::Host, body: json!({"sdp": "offer"}) }).await;
    assert_eq!(hear(&mut host).await, ServerMsg::Msg { from: 1, body: json!({"sdp": "offer"}) });
    say(&mut host, &ClientMsg::Send { to: Target::Member(1), body: json!({"sdp": "answer"}) }).await;
    assert_eq!(hear(&mut guest).await, ServerMsg::Msg { from: 0, body: json!({"sdp": "answer"}) });

    // The guest drops; the host sees it, and the guest can take its place back.
    drop(guest);
    assert_eq!(hear(&mut host).await, ServerMsg::Presence { id: 1, online: false });
}

#[tokio::test]
async fn a_guest_cannot_open_a_room_and_a_bad_path_is_refused() {
    let addr = start().await;
    let mut guest = connect(addr, "nobody").await;
    say(&mut guest, &hello("Ann", None)).await;
    assert!(matches!(hear(&mut guest).await, ServerMsg::Error { code: ErrorCode::NoRoom, .. }));

    assert!(tokio_tungstenite::connect_async(format!("ws://{addr}/elsewhere")).await.is_err());
}

#[tokio::test]
async fn ending_the_jam_closes_everyone() {
    let addr = start().await;
    let mut host = connect(addr, "endme1").await;
    say(&mut host, &hello("Hana", Some(KEY))).await;
    hear(&mut host).await;
    let mut guest = connect(addr, "endme1").await;
    say(&mut guest, &hello("Ann", None)).await;
    hear(&mut guest).await;
    hear(&mut host).await;

    say(&mut host, &ClientMsg::End).await;
    assert!(matches!(hear(&mut guest).await, ServerMsg::Closed { .. }));
}
