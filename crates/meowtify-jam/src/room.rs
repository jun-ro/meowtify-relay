//! The logic of one room, with no I/O and no clock.
//!
//! A shell (a Cloudflare Durable Object, or a native server) owns the sockets. It numbers its
//! connections, hands each incoming frame to [`Room::frame`], and carries out the returned
//! [`Out`] list. Time and randomness come in through [`Env`], so the room is deterministic in
//! tests and compiles for `wasm32`. The room is serializable: a Durable Object that sleeps
//! saves it and loads it again.

use serde::{Deserialize, Serialize};

use crate::wire::{
    ClientMsg, ErrorCode, LeftReason, Member, MemberId, ServerMsg, Target, MAX_FRAME, MAX_NAME,
};

/// A connection, as the shell numbers it. Stable while the socket lives.
pub type ConnId = u64;

/// What the room needs from the outside.
pub trait Env {
    fn now_ms(&self) -> u64;
    /// Random bits for tokens. Must be unpredictable on a public relay.
    fn random(&mut self) -> u64;
}

/// What the shell must do after a call.
#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    Send(ConnId, String),
    /// Close the connection (after any `Send` before it in the list).
    Close(ConnId),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Guests, not counting the host.
    pub max_guests: u32,
    /// How long a guest keeps its place after its connection drops.
    pub guest_grace_ms: u64,
    /// How long the host may be away before the room ends.
    pub host_grace_ms: u64,
    /// Frames a member may send in a burst, and per second after that.
    pub burst: u32,
    pub per_sec: u32,
}

impl Default for Config {
    fn default() -> Config {
        Config { max_guests: 8, guest_grace_ms: 60_000, host_grace_ms: 120_000, burst: 40, per_sec: 20 }
    }
}

/// A token bucket.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Bucket {
    level: f64,
    at_ms: u64,
}

impl Bucket {
    fn take(&mut self, now: u64, cfg: &Config) -> bool {
        let cap = cfg.burst as f64;
        let gained = now.saturating_sub(self.at_ms) as f64 / 1000.0 * cfg.per_sec as f64;
        self.level = (self.level + gained).min(cap);
        self.at_ms = now;
        if self.level >= 1.0 {
            self.level -= 1.0;
            true
        } else {
            false
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Rec {
    id: MemberId,
    name: String,
    token: String,
    host: bool,
    conn: Option<ConnId>,
    /// When the connection dropped. `None` while online.
    offline_since: Option<u64>,
    bucket: Bucket,
    #[serde(default)]
    hue: Option<u16>,
}

impl Rec {
    fn member(&self) -> Member {
        Member { id: self.id, name: self.name.clone(), host: self.host, online: self.conn.is_some(), hue: self.hue }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Room {
    cfg: Config,
    /// Empty until the host creates the room.
    host_key: String,
    members: Vec<Rec>,
    next_id: MemberId,
    closed: bool,
}

impl Room {
    pub fn new(cfg: Config) -> Room {
        Room { cfg, ..Room::default() }
    }

    /// The room has ended; the shell may forget it.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// No host has created the room yet.
    pub fn is_new(&self) -> bool {
        self.host_key.is_empty() && !self.closed
    }

    pub fn members(&self) -> Vec<Member> {
        self.members.iter().map(Rec::member).collect()
    }

    /// When `tick` next has work, in the clock of `Env::now_ms`. The shell sets its alarm here.
    pub fn next_deadline(&self) -> Option<u64> {
        self.members
            .iter()
            .filter_map(|m| m.offline_since.map(|t| t + if m.host { self.cfg.host_grace_ms } else { self.cfg.guest_grace_ms }))
            .min()
    }

    /// One frame arrived on `conn`.
    pub fn frame(&mut self, env: &mut dyn Env, conn: ConnId, text: &str) -> Vec<Out> {
        let mut out = Vec::new();
        let joined = self.index_of(conn).is_some();
        if self.closed {
            send(&mut out, conn, &ServerMsg::Closed { reason: "the jam is over".into() });
            out.push(Out::Close(conn));
            return out;
        }
        if text.len() > MAX_FRAME {
            fail(&mut out, conn, ErrorCode::TooLarge, "message too large", !joined);
            return out;
        }
        let Ok(msg) = serde_json::from_str::<ClientMsg>(text) else {
            fail(&mut out, conn, ErrorCode::BadFrame, "not a message", !joined);
            return out;
        };
        let Some(i) = self.index_of(conn) else {
            match msg {
                ClientMsg::Hello { name, host_key, resume, hue } => self.hello(env, conn, name, host_key, resume, hue, &mut out),
                _ => fail(&mut out, conn, ErrorCode::NotJoined, "say hello first", true),
            }
            return out;
        };
        if !self.members[i].bucket.take(env.now_ms(), &self.cfg) {
            fail(&mut out, conn, ErrorCode::RateLimited, "slow down", false);
            return out;
        }
        let me = self.members[i].id;
        let is_host = self.members[i].host;
        match msg {
            ClientMsg::Hello { .. } => fail(&mut out, conn, ErrorCode::AlreadyJoined, "already in the room", false),
            ClientMsg::Ping => send(&mut out, conn, &ServerMsg::Pong),
            ClientMsg::Send { to, body } => {
                let reply = ServerMsg::Msg { from: me, body };
                match (to, is_host) {
                    (Target::Host, false) => match self.members.iter().find(|m| m.host).and_then(|m| m.conn) {
                        Some(host) => send(&mut out, host, &reply),
                        None => fail(&mut out, conn, ErrorCode::Offline, "the host is away", false),
                    },
                    (Target::All, true) => {
                        for m in self.members.iter().filter(|m| m.id != me) {
                            if let Some(c) = m.conn {
                                send(&mut out, c, &reply);
                            }
                        }
                    }
                    (Target::Member(id), true) => match self.members.iter().find(|m| m.id == id).and_then(|m| m.conn) {
                        Some(c) if id != me => send(&mut out, c, &reply),
                        Some(_) => fail(&mut out, conn, ErrorCode::Forbidden, "not to yourself", false),
                        None => fail(&mut out, conn, ErrorCode::Offline, "that member is away", false),
                    },
                    _ => fail(&mut out, conn, ErrorCode::Forbidden, "not allowed", false),
                }
            }
            ClientMsg::Kick { id } => {
                let target = self.members.iter().position(|m| m.id == id && !m.host);
                match (is_host, target) {
                    (true, Some(t)) => {
                        let gone = self.members.remove(t);
                        if let Some(c) = gone.conn {
                            send(&mut out, c, &ServerMsg::Left { id, reason: LeftReason::Kicked });
                            out.push(Out::Close(c));
                        }
                        self.broadcast(&mut out, None, &ServerMsg::Left { id, reason: LeftReason::Kicked });
                    }
                    (true, None) => fail(&mut out, conn, ErrorCode::Forbidden, "no such guest", false),
                    (false, _) => fail(&mut out, conn, ErrorCode::Forbidden, "only the host can do this", false),
                }
            }
            ClientMsg::End => {
                if is_host {
                    self.close_room(&mut out, "the host ended the jam");
                } else {
                    fail(&mut out, conn, ErrorCode::Forbidden, "only the host can do this", false);
                }
            }
        }
        out
    }

    /// The socket of `conn` closed.
    pub fn disconnect(&mut self, env: &mut dyn Env, conn: ConnId) -> Vec<Out> {
        let mut out = Vec::new();
        let Some(i) = self.index_of(conn) else { return out };
        self.members[i].conn = None;
        self.members[i].offline_since = Some(env.now_ms());
        let id = self.members[i].id;
        self.broadcast(&mut out, Some(id), &ServerMsg::Presence { id, online: false });
        out
    }

    /// Ends the stays that have lasted too long. Call it at `next_deadline`.
    pub fn tick(&mut self, env: &mut dyn Env) -> Vec<Out> {
        let mut out = Vec::new();
        let now = env.now_ms();
        let (host_grace, guest_grace) = (self.cfg.host_grace_ms, self.cfg.guest_grace_ms);
        let due = |m: &Rec| m.offline_since.is_some_and(|t| now >= t + if m.host { host_grace } else { guest_grace });
        if self.members.iter().any(|m| m.host && due(m)) {
            self.close_room(&mut out, "the host did not come back");
            return out;
        }
        let gone: Vec<MemberId> = self.members.iter().filter(|m| due(m)).map(|m| m.id).collect();
        self.members.retain(|m| !gone.contains(&m.id));
        for id in gone {
            self.broadcast(&mut out, None, &ServerMsg::Left { id, reason: LeftReason::Gone });
        }
        out
    }

    fn hello(
        &mut self,
        env: &mut dyn Env,
        conn: ConnId,
        name: String,
        host_key: Option<String>,
        resume: Option<crate::wire::Resume>,
        hue: Option<u16>,
        out: &mut Vec<Out>,
    ) {
        let now = env.now_ms();
        let hue = hue.filter(|h| *h < 360);
        let i = match host_key {
            Some(key) => {
                if self.host_key.is_empty() {
                    if !(16..=128).contains(&key.len()) {
                        return fail(out, conn, ErrorCode::BadKey, "the host key must be 16 to 128 characters", true);
                    }
                    self.host_key = key;
                    let token = token(env);
                    let id = self.take_id();
                    self.members.push(Rec { id, name: clean_name(&name, id), token, host: true, conn: None, offline_since: None, bucket: Bucket { level: self.cfg.burst as f64, at_ms: now }, hue });
                } else if !same(&key, &self.host_key) {
                    return fail(out, conn, ErrorCode::BadKey, "wrong host key", true);
                }
                self.members.iter().position(|m| m.host).expect("a created room has a host")
            }
            None if self.host_key.is_empty() => return fail(out, conn, ErrorCode::NoRoom, "no jam with this code", true),
            None => {
                let back = resume.and_then(|r| self.members.iter().position(|m| !m.host && m.id == r.id && same(&m.token, &r.token)));
                match back {
                    Some(i) => i,
                    None => {
                        if self.members.iter().filter(|m| !m.host).count() >= self.cfg.max_guests as usize {
                            return fail(out, conn, ErrorCode::Full, "the jam is full", true);
                        }
                        let token = token(env);
                        let id = self.take_id();
                        self.members.push(Rec { id, name: clean_name(&name, id), token, host: false, conn: None, offline_since: None, bucket: Bucket { level: self.cfg.burst as f64, at_ms: now }, hue });
                        self.members.len() - 1
                    }
                }
            }
        };
        // A second connection for the same place replaces the first.
        let fresh = self.members[i].conn.is_none() && self.members[i].offline_since.is_none();
        if let Some(old) = self.members[i].conn.take() {
            out.push(Out::Close(old));
        }
        self.members[i].conn = Some(conn);
        self.members[i].offline_since = None;
        let rec = self.members[i].clone();
        send(
            out,
            conn,
            &ServerMsg::Welcome { you: rec.id, host: rec.host, token: rec.token.clone(), members: self.members(), max_guests: self.cfg.max_guests },
        );
        let news = if fresh && !rec.host { ServerMsg::Joined { member: rec.member() } } else { ServerMsg::Presence { id: rec.id, online: true } };
        self.broadcast(out, Some(rec.id), &news);
    }

    fn take_id(&mut self) -> MemberId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn index_of(&self, conn: ConnId) -> Option<usize> {
        self.members.iter().position(|m| m.conn == Some(conn))
    }

    /// Sends `msg` to every member that is online, except `except`.
    fn broadcast(&self, out: &mut Vec<Out>, except: Option<MemberId>, msg: &ServerMsg) {
        for m in self.members.iter().filter(|m| Some(m.id) != except) {
            if let Some(c) = m.conn {
                send(out, c, msg);
            }
        }
    }

    fn close_room(&mut self, out: &mut Vec<Out>, reason: &str) {
        for m in &self.members {
            if let Some(c) = m.conn {
                send(out, c, &ServerMsg::Closed { reason: reason.into() });
                out.push(Out::Close(c));
            }
        }
        self.members.clear();
        self.host_key.clear();
        self.closed = true;
    }
}

fn send(out: &mut Vec<Out>, conn: ConnId, msg: &ServerMsg) {
    if let Ok(text) = serde_json::to_string(msg) {
        out.push(Out::Send(conn, text));
    }
}

fn fail(out: &mut Vec<Out>, conn: ConnId, code: ErrorCode, message: &str, close: bool) {
    send(out, conn, &ServerMsg::Error { code, message: message.into() });
    if close {
        out.push(Out::Close(conn));
    }
}

fn token(env: &mut dyn Env) -> String {
    format!("{:016x}{:016x}", env.random(), env.random())
}

/// Equal strings, without stopping at the first difference.
fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= (*a.get(i).unwrap_or(&0) ^ *b.get(i).unwrap_or(&0)) as usize;
    }
    diff == 0
}

/// A display name with no control characters, at most `MAX_NAME` characters.
fn clean_name(raw: &str, id: MemberId) -> String {
    let name: String = raw.chars().filter(|c| !c.is_control()).collect();
    let name: String = name.trim().chars().take(MAX_NAME).collect();
    if name.is_empty() {
        if id == 0 { "Host".into() } else { format!("Guest {id}") }
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Fake {
        now: u64,
        n: u64,
    }
    impl Env for Fake {
        fn now_ms(&self) -> u64 {
            self.now
        }
        fn random(&mut self) -> u64 {
            self.n += 1;
            self.n.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        }
    }

    const KEY: &str = "0123456789abcdef0123";

    fn env() -> Fake {
        Fake { now: 1_000, n: 0 }
    }

    fn hello(name: &str, key: Option<&str>) -> String {
        json!({"t": "hello", "name": name, "host_key": key}).to_string()
    }

    fn msgs(out: &[Out], conn: ConnId) -> Vec<ServerMsg> {
        out.iter()
            .filter_map(|o| match o {
                Out::Send(c, t) if *c == conn => serde_json::from_str(t).ok(),
                _ => None,
            })
            .collect()
    }

    fn closed(out: &[Out], conn: ConnId) -> bool {
        out.contains(&Out::Close(conn))
    }

    /// A room with the host on connection 1 and a guest on connection 2.
    fn jam() -> (Room, Fake, String) {
        let mut room = Room::new(Config::default());
        let mut e = env();
        room.frame(&mut e, 1, &hello("Hana", Some(KEY)));
        let out = room.frame(&mut e, 2, &hello("Ann", None));
        let token = match &msgs(&out, 2)[0] {
            ServerMsg::Welcome { token, .. } => token.clone(),
            other => panic!("{other:?}"),
        };
        (room, e, token)
    }

    #[test]
    fn the_host_creates_the_room_and_a_guest_joins_it() {
        let mut room = Room::new(Config::default());
        let mut e = env();
        assert!(room.is_new());
        let out = room.frame(&mut e, 2, &hello("Ann", None));
        assert!(matches!(&msgs(&out, 2)[0], ServerMsg::Error { code: ErrorCode::NoRoom, .. }));
        assert!(closed(&out, 2));

        let out = room.frame(&mut e, 1, &hello("Hana", Some(KEY)));
        assert!(matches!(&msgs(&out, 1)[0], ServerMsg::Welcome { you: 0, host: true, .. }));
        let out = room.frame(&mut e, 2, &hello("Ann", None));
        let ServerMsg::Welcome { you, host, members, .. } = &msgs(&out, 2)[0] else { panic!() };
        assert_eq!((*you, *host, members.len()), (1, false, 2));
        assert!(matches!(&msgs(&out, 1)[0], ServerMsg::Joined { member } if member.name == "Ann"), "the host is told");
    }

    #[test]
    fn a_guest_writes_to_the_host_only() {
        let (mut room, mut e, _) = jam();
        let body = json!({"sdp": "offer"});
        let send = |to: Target| json!({"t": "send", "to": to, "body": body}).to_string();
        let out = room.frame(&mut e, 2, &send(Target::Host));
        assert_eq!(msgs(&out, 1), vec![ServerMsg::Msg { from: 1, body: body.clone() }]);
        for to in [Target::All, Target::Member(0)] {
            let out = room.frame(&mut e, 2, &send(to));
            assert!(matches!(&msgs(&out, 2)[0], ServerMsg::Error { code: ErrorCode::Forbidden, .. }), "{to:?}");
            assert!(msgs(&out, 1).is_empty());
        }
    }

    #[test]
    fn the_host_writes_to_one_or_to_all() {
        let (mut room, mut e, _) = jam();
        room.frame(&mut e, 3, &hello("Bo", None));
        let send = |to: Target| json!({"t": "send", "to": to, "body": 7}).to_string();
        let out = room.frame(&mut e, 1, &send(Target::All));
        assert_eq!((msgs(&out, 2).len(), msgs(&out, 3).len(), msgs(&out, 1).len()), (1, 1, 0));
        let out = room.frame(&mut e, 1, &send(Target::Member(2)));
        assert_eq!((msgs(&out, 2).len(), msgs(&out, 3).len()), (0, 1));
        let out = room.frame(&mut e, 1, &send(Target::Member(9)));
        assert!(matches!(&msgs(&out, 1)[0], ServerMsg::Error { code: ErrorCode::Offline, .. }));
    }

    #[test]
    fn a_dropped_guest_comes_back_with_its_token_and_loses_the_place_after_the_grace() {
        let (mut room, mut e, token) = jam();
        let out = room.disconnect(&mut e, 2);
        assert!(matches!(&msgs(&out, 1)[0], ServerMsg::Presence { id: 1, online: false }));
        assert_eq!(room.next_deadline(), Some(1_000 + 60_000));

        e.now += 30_000;
        let resume = json!({"t": "hello", "name": "Ann", "resume": {"id": 1, "token": token}}).to_string();
        let out = room.frame(&mut e, 5, &resume);
        assert!(matches!(&msgs(&out, 5)[0], ServerMsg::Welcome { you: 1, .. }), "same place");
        assert!(matches!(&msgs(&out, 1)[0], ServerMsg::Presence { id: 1, online: true }));
        assert_eq!(room.next_deadline(), None);

        room.disconnect(&mut e, 5);
        e.now += 60_000;
        let out = room.tick(&mut e);
        assert!(matches!(&msgs(&out, 1)[0], ServerMsg::Left { id: 1, reason: LeftReason::Gone }));
        assert_eq!(room.members().len(), 1);
    }

    #[test]
    fn a_wrong_token_is_a_new_guest() {
        let (mut room, mut e, _) = jam();
        room.disconnect(&mut e, 2);
        let resume = json!({"t": "hello", "name": "Mallory", "resume": {"id": 1, "token": "guess"}}).to_string();
        let out = room.frame(&mut e, 6, &resume);
        assert!(matches!(&msgs(&out, 6)[0], ServerMsg::Welcome { you: 2, .. }), "a fresh place, not the old one");
    }

    #[test]
    fn the_host_needs_its_key_and_the_room_ends_when_it_stays_away() {
        let (mut room, mut e, _) = jam();
        let out = room.frame(&mut e, 9, &hello("Mallory", Some("ffffffffffffffffffff")));
        assert!(matches!(&msgs(&out, 9)[0], ServerMsg::Error { code: ErrorCode::BadKey, .. }));
        assert!(closed(&out, 9));

        room.disconnect(&mut e, 1);
        e.now += 60_000;
        assert!(room.tick(&mut e).is_empty(), "guests wait for the host");
        e.now += 60_000;
        let out = room.tick(&mut e);
        assert!(matches!(&msgs(&out, 2)[0], ServerMsg::Closed { .. }));
        assert!(closed(&out, 2) && room.is_closed());
    }

    #[test]
    fn the_host_takes_its_place_back_and_replaces_a_stale_connection() {
        let (mut room, mut e, _) = jam();
        let out = room.frame(&mut e, 7, &hello("Hana", Some(KEY)));
        assert!(closed(&out, 1), "the old connection of the host is closed");
        assert!(matches!(&msgs(&out, 7)[0], ServerMsg::Welcome { you: 0, host: true, .. }));
        assert!(!room.is_closed());
    }

    #[test]
    fn the_jam_has_a_guest_limit() {
        let mut room = Room::new(Config { max_guests: 2, ..Config::default() });
        let mut e = env();
        room.frame(&mut e, 1, &hello("Hana", Some(KEY)));
        room.frame(&mut e, 2, &hello("A", None));
        room.frame(&mut e, 3, &hello("B", None));
        let out = room.frame(&mut e, 4, &hello("C", None));
        assert!(matches!(&msgs(&out, 4)[0], ServerMsg::Error { code: ErrorCode::Full, .. }));
        assert!(closed(&out, 4));
    }

    #[test]
    fn only_the_host_kicks_and_ends() {
        let (mut room, mut e, _) = jam();
        let kick = json!({"t": "kick", "id": 1}).to_string();
        let out = room.frame(&mut e, 2, &json!({"t": "kick", "id": 0}).to_string());
        assert!(matches!(&msgs(&out, 2)[0], ServerMsg::Error { code: ErrorCode::Forbidden, .. }));
        let out = room.frame(&mut e, 2, &json!({"t": "end"}).to_string());
        assert!(matches!(&msgs(&out, 2)[0], ServerMsg::Error { code: ErrorCode::Forbidden, .. }));

        let out = room.frame(&mut e, 1, &kick);
        assert!(matches!(&msgs(&out, 2)[0], ServerMsg::Left { id: 1, reason: LeftReason::Kicked }));
        assert!(closed(&out, 2));
        assert_eq!(room.members().len(), 1);

        let out = room.frame(&mut e, 1, &json!({"t": "end"}).to_string());
        assert!(closed(&out, 1) && room.is_closed());
        let out = room.frame(&mut e, 8, &hello("Late", None));
        assert!(matches!(&msgs(&out, 8)[0], ServerMsg::Closed { .. }));
    }

    #[test]
    fn bad_input_is_refused() {
        let (mut room, mut e, _) = jam();
        let out = room.frame(&mut e, 2, "{");
        assert!(matches!(&msgs(&out, 2)[0], ServerMsg::Error { code: ErrorCode::BadFrame, .. }));
        assert!(!closed(&out, 2), "a joined member stays after one bad frame");
        let out = room.frame(&mut e, 2, &"x".repeat(MAX_FRAME + 1));
        assert!(matches!(&msgs(&out, 2)[0], ServerMsg::Error { code: ErrorCode::TooLarge, .. }));
        let out = room.frame(&mut e, 2, &hello("Ann", None));
        assert!(matches!(&msgs(&out, 2)[0], ServerMsg::Error { code: ErrorCode::AlreadyJoined, .. }));
        let out = room.frame(&mut e, 50, &json!({"t": "ping"}).to_string());
        assert!(matches!(&msgs(&out, 50)[0], ServerMsg::Error { code: ErrorCode::NotJoined, .. }));
        assert!(closed(&out, 50));
        let out = room.frame(&mut e, 51, &hello("Mallory", Some("short")));
        assert!(matches!(&msgs(&out, 51)[0], ServerMsg::Error { code: ErrorCode::BadKey, .. }));
    }

    #[test]
    fn a_flood_is_slowed_and_the_bucket_refills() {
        let (mut room, mut e, _) = jam();
        let ping = json!({"t": "ping"}).to_string();
        let cfg = Config::default();
        let mut limited = 0;
        for _ in 0..cfg.burst + 10 {
            let out = room.frame(&mut e, 2, &ping);
            if matches!(&msgs(&out, 2)[0], ServerMsg::Error { code: ErrorCode::RateLimited, .. }) {
                limited += 1;
            }
        }
        assert!(limited >= 9, "{limited}");
        e.now += 1_000;
        let out = room.frame(&mut e, 2, &ping);
        assert_eq!(msgs(&out, 2), vec![ServerMsg::Pong]);
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_name("  Ann\u{0}\n ", 1), "Ann");
        assert_eq!(clean_name("   ", 4), "Guest 4");
        assert_eq!(clean_name("", 0), "Host");
        assert_eq!(clean_name(&"x".repeat(100), 1).chars().count(), MAX_NAME);
        assert!(same("abc", "abc") && !same("abc", "abd") && !same("abc", "abcd"));
    }

    #[test]
    fn a_sleeping_room_survives_being_saved() {
        let (room, mut e, token) = jam();
        let saved = serde_json::to_string(&room).unwrap();
        let mut room: Room = serde_json::from_str(&saved).unwrap();
        room.disconnect(&mut e, 2);
        let resume = json!({"t": "hello", "name": "Ann", "resume": {"id": 1, "token": token}}).to_string();
        let out = room.frame(&mut e, 9, &resume);
        assert!(matches!(&msgs(&out, 9)[0], ServerMsg::Welcome { you: 1, .. }));
    }
}
