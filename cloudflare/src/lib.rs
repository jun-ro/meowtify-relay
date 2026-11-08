//! The jam relay on Cloudflare Workers.
//!
//! `/jam/<code>` upgrades to a WebSocket that belongs to one Durable Object per room, which runs
//! the room logic of `meowtify-jam`. The object uses the WebSocket Hibernation API, so a quiet
//! room costs nothing: it saves its state, sleeps, and wakes on the next message or alarm.
//! `/turn` hands out ICE servers, with short-lived TURN credentials when Cloudflare Realtime
//! TURN is set up.

use meowtify_jam::{room_code, Config, ConnId, Env as JamEnv, Out, Room};
use serde::{Deserialize, Serialize};
use worker::*;

/// Where the room is kept between wake-ups.
const ROOM_KEY: &str = "room";
/// How long a TURN credential lives.
const TURN_TTL_SECS: u32 = 4 * 3600;

struct Runtime;

impl JamEnv for Runtime {
    fn now_ms(&self) -> u64 {
        Date::now().as_millis()
    }
    fn random(&mut self) -> u64 {
        random_u64()
    }
}

/// Random bits from the Web Crypto API of the Workers runtime.
fn random_u64() -> u64 {
    use js_sys::{Function, Reflect, Uint8Array};
    use worker::wasm_bindgen::JsCast;
    let bytes = Uint8Array::new_with_length(8);
    let crypto = Reflect::get(&js_sys::global(), &"crypto".into()).expect("Workers have crypto");
    let fill: Function = Reflect::get(&crypto, &"getRandomValues".into()).expect("crypto fills arrays").unchecked_into();
    fill.call1(&crypto, &bytes).expect("crypto fills the array");
    let mut raw = [0u8; 8];
    bytes.copy_to(&mut raw);
    u64::from_le_bytes(raw)
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let path = req.path();
    if path == "/turn" {
        return turn(&env).await;
    }
    if let Some(code) = room_code(&path) {
        if req.headers().get("Upgrade")?.as_deref() != Some("websocket") {
            return Response::error("Expected a WebSocket", 426);
        }
        return env.durable_object("JAM_ROOM")?.get_by_name(&code)?.fetch_with_request(req).await;
    }
    Response::ok("Meowtify jam relay")
}

/// ICE servers for a peer to peer connection. Without TURN credentials set up, STUN only:
/// that connects most pairs, and a self-hoster can add coturn the same way.
async fn turn(env: &Env) -> Result<Response> {
    let stun_only = serde_json::json!({ "iceServers": [{ "urls": ["stun:stun.cloudflare.com:3478"] }] });
    let (Ok(key), Ok(token)) = (env.secret("TURN_KEY_ID"), env.secret("TURN_API_TOKEN")) else {
        return Response::from_json(&stun_only);
    };
    let url = format!("https://rtc.live.cloudflare.com/v1/turn/keys/{key}/credentials/generate-ice-servers");
    let headers = Headers::new();
    headers.set("Authorization", &format!("Bearer {token}"))?;
    headers.set("Content-Type", "application/json")?;
    let body = serde_json::json!({ "ttl": TURN_TTL_SECS }).to_string();
    let mut init = RequestInit::new();
    init.with_method(Method::Post).with_headers(headers).with_body(Some(body.into()));
    let answer = Fetch::Request(Request::new_with_init(&url, &init)?).send().await;
    match answer {
        Ok(mut r) if r.status_code() < 300 => match r.json::<serde_json::Value>().await {
            Ok(v) => Response::from_json(&v),
            Err(_) => Response::from_json(&stun_only),
        },
        _ => Response::from_json(&stun_only),
    }
}

/// What a socket remembers across a sleep: which connection of the room it is.
#[derive(Serialize, Deserialize)]
struct Attachment {
    conn: ConnId,
}

fn conn_of(ws: &WebSocket) -> Option<ConnId> {
    ws.deserialize_attachment::<Attachment>().ok().flatten().map(|a| a.conn)
}

#[durable_object]
pub struct JamRoom {
    state: State,
}

impl JamRoom {
    async fn load(&self) -> Room {
        self.state.storage().get::<Room>(ROOM_KEY).await.ok().flatten().unwrap_or_else(|| Room::new(Config::default()))
    }

    /// A socket closed or failed.
    async fn dropped(&self, ws: &WebSocket) -> Result<()> {
        let Some(conn) = conn_of(ws) else { return Ok(()) };
        let mut room = self.load().await;
        let outs = room.disconnect(&mut Runtime, conn);
        self.finish(room, outs).await
    }

    /// Carries out what the room asked for, then saves it and sets the next alarm.
    async fn finish(&self, room: Room, outs: Vec<Out>) -> Result<()> {
        let sockets = self.state.get_websockets();
        for out in outs {
            match out {
                Out::Send(c, text) => {
                    if let Some(ws) = sockets.iter().find(|w| conn_of(w) == Some(c)) {
                        let _ = ws.send_with_str(text);
                    }
                }
                Out::Close(c) => {
                    if let Some(ws) = sockets.iter().find(|w| conn_of(w) == Some(c)) {
                        let _ = ws.close(Some(1000), Some("bye"));
                    }
                }
            }
        }
        let storage = self.state.storage();
        if room.is_closed() || (room.is_new() && sockets.is_empty()) {
            storage.delete_all().await?;
            return Ok(());
        }
        storage.put(ROOM_KEY, &room).await?;
        if let Some(due) = room.next_deadline() {
            let wait = due.saturating_sub(Runtime.now_ms()).max(1);
            storage.set_alarm(wait as i64).await?;
        }
        Ok(())
    }
}

impl DurableObject for JamRoom {
    fn new(state: State, _env: Env) -> Self {
        JamRoom { state }
    }

    async fn fetch(&self, _req: Request) -> Result<Response> {
        let pair = WebSocketPair::new()?;
        self.state.accept_web_socket(&pair.server);
        pair.server.serialize_attachment(Attachment { conn: random_u64() })?;
        Response::from_websocket(pair.client)
    }

    async fn websocket_message(&self, ws: WebSocket, message: WebSocketIncomingMessage) -> Result<()> {
        let (WebSocketIncomingMessage::String(text), Some(conn)) = (message, conn_of(&ws)) else { return Ok(()) };
        let mut room = self.load().await;
        let outs = room.frame(&mut Runtime, conn, &text);
        self.finish(room, outs).await
    }

    async fn websocket_close(&self, ws: WebSocket, _code: usize, _reason: String, _was_clean: bool) -> Result<()> {
        self.dropped(&ws).await
    }

    async fn websocket_error(&self, ws: WebSocket, _error: Error) -> Result<()> {
        self.dropped(&ws).await
    }

    async fn alarm(&self) -> Result<Response> {
        let mut room = self.load().await;
        let outs = room.tick(&mut Runtime);
        self.finish(room, outs).await?;
        Response::ok("")
    }
}
