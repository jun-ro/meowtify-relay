//! The messages between a client and the relay, as JSON text frames.
//!
//! The relay forwards a `body` it never looks into: the offers, answers and ICE candidates
//! that set up a direct connection. Audio and playback state do not pass through the relay.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A member's number in a room. The host is always 0.
pub type MemberId = u32;

/// The largest frame the relay accepts. An SDP offer is a few kilobytes.
pub const MAX_FRAME: usize = 16 * 1024;
/// Longest display name, in characters.
pub const MAX_NAME: usize = 32;

/// Who a message is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Host,
    /// Every member that is online, except the sender. Host only.
    All,
    /// One member. Host only.
    Member(MemberId),
}

/// Proof of an earlier visit, so a dropped guest takes its place back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resume {
    pub id: MemberId,
    pub token: String,
}

/// Client to relay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMsg {
    /// The first frame of every connection. With `host_key` it creates the room, or takes the
    /// host's place back. With `resume` a guest takes its old place back.
    Hello {
        name: String,
        #[serde(default)]
        host_key: Option<String>,
        #[serde(default)]
        resume: Option<Resume>,
        /// The colour of the member's avatar, in degrees. `None` lets each peer derive it from the name.
        #[serde(default)]
        hue: Option<u16>,
    },
    /// Passes `body` to `to`. Guests may only write to the host.
    Send { to: Target, body: Value },
    /// Removes a guest. Host only.
    Kick { id: MemberId },
    /// Closes the room for everyone. Host only.
    End,
    Ping,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub id: MemberId,
    pub name: String,
    pub host: bool,
    pub online: bool,
    /// The avatar colour the member chose, in degrees from 0 to 359.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hue: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeftReason {
    /// Away for longer than the grace period.
    Gone,
    Kicked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadFrame,
    TooLarge,
    /// No room has this code.
    NoRoom,
    /// The host key does not match, or is not usable.
    BadKey,
    Full,
    /// The sender may not do this.
    Forbidden,
    /// The receiver is not connected right now.
    Offline,
    RateLimited,
    /// A message came before `hello`.
    NotJoined,
    AlreadyJoined,
}

/// Relay to client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    /// The answer to `hello`. Keep `token` to come back with `resume`.
    Welcome { you: MemberId, host: bool, token: String, members: Vec<Member>, max_guests: u32 },
    Joined { member: Member },
    /// A member connected or lost its connection. It keeps its place for a while.
    Presence { id: MemberId, online: bool },
    Left { id: MemberId, reason: LeftReason },
    Msg { from: MemberId, body: Value },
    Error { code: ErrorCode, message: String },
    /// The room is over: the host ended it, or stayed away too long.
    Closed { reason: String },
    Pong,
}

/// Length of a room code, in characters.
pub const CODE_LEN: std::ops::RangeInclusive<usize> = 6..=32;

/// The room a URL path names: `/jam/<code>`, letters and digits only. Codes ignore case.
pub fn room_code(path: &str) -> Option<String> {
    let code = path.strip_prefix("/jam/")?;
    let ok = CODE_LEN.contains(&code.len()) && code.chars().all(|c| c.is_ascii_alphanumeric());
    ok.then(|| code.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_codes_name_a_room() {
        assert_eq!(room_code("/jam/AbC123").as_deref(), Some("abc123"));
        assert_eq!(room_code("/jam/abc"), None, "too short");
        assert_eq!(room_code("/jam/../etc/passwd"), None);
        assert_eq!(room_code("/other/abcdef"), None);
        assert_eq!(room_code("/jam/abc-def"), None);
    }

    #[test]
    fn frames_have_a_stable_shape() {
        let m: ClientMsg = serde_json::from_str(r#"{"t":"send","to":"host","body":{"sdp":"x"}}"#).unwrap();
        assert_eq!(m, ClientMsg::Send { to: Target::Host, body: serde_json::json!({"sdp": "x"}) });
        let m: ClientMsg = serde_json::from_str(r#"{"t":"send","to":{"member":3},"body":1}"#).unwrap();
        assert!(matches!(m, ClientMsg::Send { to: Target::Member(3), .. }));
        let m: ClientMsg = serde_json::from_str(r#"{"t":"hello","name":"Ann"}"#).unwrap();
        assert!(matches!(m, ClientMsg::Hello { host_key: None, resume: None, .. }));
        assert_eq!(serde_json::to_string(&ServerMsg::Pong).unwrap(), r#"{"t":"pong"}"#);
        assert!(serde_json::from_str::<ClientMsg>(r#"{"t":"nope"}"#).is_err());
    }
}
