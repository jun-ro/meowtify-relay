//! A self-hosted relay for jam sessions.
//!
//! It accepts WebSocket connections at `/jam/<code>`, one room per code, and runs the shared
//! room logic of `meowtify-jam` on them. It carries only the small messages that introduce the
//! host and the guests to each other. The Cloudflare Worker in `relay/cloudflare` does the
//! same job on Cloudflare's network.

pub mod turn_server;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use meowtify_jam::wire::MAX_FRAME;
use meowtify_jam::{room_code, Config, ConnId, Env, Out, Room};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

/// A connection with nothing heard for this long is dropped. Clients ping well within it.
const IDLE: Duration = Duration::from_secs(120);
/// Rooms the relay keeps at once. A public relay should not be a place to store state.
pub const MAX_ROOMS: usize = 500;

#[derive(Clone)]
pub struct Options {
    pub room: Config,
    pub max_rooms: usize,
    /// The TURN server that `GET /turn` hands out credentials for, if this relay runs one.
    pub turn: Option<Arc<turn_server::Turn>>,
}

impl Default for Options {
    fn default() -> Options {
        Options { room: Config::default(), max_rooms: MAX_ROOMS, turn: None }
    }
}

struct SysEnv(Instant);

impl Env for SysEnv {
    fn now_ms(&self) -> u64 {
        self.0.elapsed().as_millis() as u64
    }
    fn random(&mut self) -> u64 {
        getrandom::u64().expect("the system has a source of randomness")
    }
}

struct Entry {
    room: Room,
    conns: HashMap<ConnId, mpsc::UnboundedSender<Message>>,
}

impl Entry {
    /// Carries out what the room asked for.
    fn dispatch(&mut self, outs: Vec<Out>) {
        for out in outs {
            match out {
                Out::Send(c, text) => {
                    if let Some(tx) = self.conns.get(&c) {
                        let _ = tx.send(Message::text(text));
                    }
                }
                Out::Close(c) => {
                    if let Some(tx) = self.conns.remove(&c) {
                        let _ = tx.send(Message::Close(None));
                    }
                }
            }
        }
    }

    fn finished(&self) -> bool {
        self.room.is_closed() || (self.room.is_new() && self.conns.is_empty())
    }
}

struct Hub {
    rooms: HashMap<String, Entry>,
    next_conn: ConnId,
    opts: Options,
    env: SysEnv,
}

type Shared = Arc<Mutex<Hub>>;

/// Serves until the listener fails. Each room lives while it has people.
pub async fn serve(listener: TcpListener, opts: Options) {
    let hub: Shared = Arc::new(Mutex::new(Hub { rooms: HashMap::new(), next_conn: 1, opts, env: SysEnv(Instant::now()) }));
    tokio::spawn(clock(hub.clone()));
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(connection(hub.clone(), stream));
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

/// Ends the stays that lasted too long, once a second.
async fn clock(hub: Shared) {
    let mut every = tokio::time::interval(Duration::from_secs(1));
    loop {
        every.tick().await;
        let mut hub = hub.lock().unwrap();
        let Hub { rooms, env, .. } = &mut *hub;
        let now = env.now_ms();
        for entry in rooms.values_mut() {
            if entry.room.next_deadline().is_some_and(|d| d <= now) {
                let outs = entry.room.tick(env);
                entry.dispatch(outs);
            }
        }
        rooms.retain(|_, e| !e.finished());
    }
}

fn refuse(status: StatusCode, why: &str) -> ErrorResponse {
    let mut r = ErrorResponse::new(Some(why.to_string()));
    *r.status_mut() = status;
    r
}

/// True when the request on `stream` is `GET /turn`. Looks without reading, so the WebSocket
/// handshake still sees every byte of any other request.
async fn is_turn_request(stream: &TcpStream) -> bool {
    const START: &[u8] = b"GET /turn";
    let mut buf = [0u8; 16];
    for _ in 0..100 {
        match stream.peek(&mut buf).await {
            Ok(n) if n >= START.len() => return buf.starts_with(START) && matches!(buf[START.len()], b' ' | b'?'),
            Ok(0) | Err(_) => return false,
            Ok(_) => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
    false
}

/// Answers `GET /turn` with the ICE servers, or 404 when this relay has no TURN server.
async fn serve_turn(mut stream: TcpStream, turn: Option<Arc<turn_server::Turn>>) {
    let mut head = Vec::new();
    let mut chunk = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 8192 {
        match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk)).await {
            Ok(Ok(n)) if n > 0 => head.extend_from_slice(&chunk[..n]),
            _ => return,
        }
    }
    let (status, body) = match turn {
        Some(t) => ("200 OK", t.ice_servers()),
        None => ("404 Not Found", String::from("this relay has no TURN server")),
    };
    let ctype = if turn_is_json(status) { "application/json" } else { "text/plain" };
    let reply = format!("HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}", body.len());
    let _ = stream.write_all(reply.as_bytes()).await;
    let _ = stream.shutdown().await;
}

fn turn_is_json(status: &str) -> bool {
    status.starts_with("200")
}

async fn connection(hub: Shared, stream: TcpStream) {
    if is_turn_request(&stream).await {
        let turn = hub.lock().unwrap().opts.turn.clone();
        return serve_turn(stream, turn).await;
    }
    let mut code = None;
    let cfg = WebSocketConfig::default().max_message_size(Some(MAX_FRAME * 2)).max_frame_size(Some(MAX_FRAME * 2));
    let ws = tokio_tungstenite::accept_hdr_async_with_config(
        stream,
        |req: &Request, resp: Response| match room_code(req.uri().path()) {
            Some(c) => {
                code = Some(c);
                Ok(resp)
            }
            None => Err(refuse(StatusCode::NOT_FOUND, "use /jam/<code>")),
        },
        Some(cfg),
    )
    .await;
    let (Ok(ws), Some(code)) = (ws, code) else { return };
    let (mut sink, mut source) = ws.split();

    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
    let admitted = {
        let mut hub = hub.lock().unwrap();
        let conn = hub.next_conn;
        hub.next_conn += 1;
        let (max, cfg) = (hub.opts.max_rooms, hub.opts.room.clone());
        if !hub.rooms.contains_key(&code) && hub.rooms.len() >= max {
            None
        } else {
            hub.rooms.entry(code.clone()).or_insert_with(|| Entry { room: Room::new(cfg), conns: HashMap::new() }).conns.insert(conn, tx);
            Some(conn)
        }
    };
    let Some(conn) = admitted else {
        let _ = sink.send(Message::Close(None)).await;
        return;
    };

    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let last = matches!(msg, Message::Close(_));
            if sink.send(msg).await.is_err() || last {
                break;
            }
        }
        let _ = sink.close().await;
    });

    while let Ok(Some(Ok(msg))) = tokio::time::timeout(IDLE, source.next()).await {
        match msg {
            Message::Text(text) => {
                let mut hub = hub.lock().unwrap();
                let Hub { rooms, env, .. } = &mut *hub;
                let Some(entry) = rooms.get_mut(&code) else { break };
                if !entry.conns.contains_key(&conn) {
                    break; // the room closed this connection
                }
                let outs = entry.room.frame(env, conn, text.as_str());
                entry.dispatch(outs);
                if entry.finished() {
                    rooms.remove(&code);
                }
            }
            Message::Close(_) => break,
            _ => {} // pings are answered by the library; binary frames are ignored
        }
    }

    {
        let mut hub = hub.lock().unwrap();
        let Hub { rooms, env, .. } = &mut *hub;
        if let Some(entry) = rooms.get_mut(&code) {
            let outs = entry.room.disconnect(env, conn);
            entry.dispatch(outs);
            entry.conns.remove(&conn);
            if entry.finished() {
                rooms.remove(&code);
            }
        }
    }
    writer.abort();
}

/// Binds `addr` and returns the listener, for the binary and for tests.
pub async fn bind(addr: SocketAddr) -> std::io::Result<TcpListener> {
    TcpListener::bind(addr).await
}
