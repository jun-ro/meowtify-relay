//! What host and guests say to each other once the direct connection is open, and what they
//! say through the relay to set it up. The relay never parses any of this.

use serde::{Deserialize, Serialize};

/// A song of the session, numbered by the host. Guests never see file paths.
pub type SongId = u32;

/// Songs listed in a state message. The rest of a long queue is not shown to guests.
pub const MAX_QUEUE: usize = 20;

/// A song as guests are told about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SongRef {
    pub id: SongId,
    pub title: String,
    pub artist: String,
    pub duration_ms: u32,
    /// The id in the song's file name (a YouTube id, say). It is public, and it lets a guest
    /// that has the same song play its own copy. The host's path is never sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// What the host plays, as of the moment `at_ms` on the host's clock.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HostState {
    /// Grows with every change, so a guest can tell a newer state from a late one.
    pub rev: u64,
    pub song: Option<SongRef>,
    pub playing: bool,
    pub pos_ms: u32,
    pub at_ms: u64,
    /// The songs after this one, in play order, at most [`MAX_QUEUE`].
    pub queue: Vec<SongRef>,
}

/// What a guest may ask the host to do. The host decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Perms {
    /// Pause, resume and seek.
    pub control: bool,
    /// Next and previous.
    pub skip: bool,
    /// Suggest songs for the queue.
    pub add: bool,
}

impl Perms {
    /// A listener who may suggest songs. The default for a new guest.
    pub const LISTENER: Perms = Perms { control: false, skip: false, add: true };
    /// Everything a guest can be given.
    pub const DJ: Perms = Perms { control: true, skip: true, add: true };
    pub const NONE: Perms = Perms { control: false, skip: false, add: false };
}

impl Default for Perms {
    fn default() -> Perms {
        Perms::LISTENER
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Action {
    Play,
    Pause,
    Seek { ms: u32 },
    Next,
    Previous,
    /// "I would like to hear this": free text the host's app turns into a search.
    Suggest { text: String },
}

impl Action {
    /// Whether `perms` allow this action.
    pub fn allowed(&self, perms: Perms) -> bool {
        match self {
            Action::Play | Action::Pause | Action::Seek { .. } => perms.control,
            Action::Next | Action::Previous => perms.skip,
            Action::Suggest { .. } => perms.add,
        }
    }
}

/// Host to guest, on the control channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ToGuest {
    State(HostState),
    /// The answer to `Ping`: the host's clock at that moment.
    Pong { sent_ms: u64, host_ms: u64 },
    Perms(Perms),
    /// All of a song has been sent.
    AudioEnd { song: SongId, size: u64 },
    /// The song is not available (it left the session, or its file is gone).
    NoSong { song: SongId },
    /// A request was refused.
    Denied { what: String },
}

/// Guest to host, on the control channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ToHost {
    Ping { sent_ms: u64 },
    /// Send the song's audio from byte `from` on. For a guest that cached a part already.
    Want { song: SongId, from: u64 },
    /// Stop sending this song.
    Cancel { song: SongId },
    Request(Action),
}

/// What goes through the relay between a host and one guest, to build their direct connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "sig", rename_all = "snake_case")]
pub enum Signal {
    Offer { sdp: String },
    Answer { sdp: String },
    Candidate { json: String },
}

/// Bytes before the audio in every audio message: the song and the offset of the first byte.
pub const AUDIO_HEADER: usize = 12;

pub fn audio_frame(song: SongId, offset: u64, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(AUDIO_HEADER + data.len());
    out.extend_from_slice(&song.to_be_bytes());
    out.extend_from_slice(&offset.to_be_bytes());
    out.extend_from_slice(data);
    out
}

/// Splits an audio message into its song, offset and bytes.
pub fn parse_audio_frame(frame: &[u8]) -> Option<(SongId, u64, &[u8])> {
    let head = frame.get(..AUDIO_HEADER)?;
    let song = SongId::from_be_bytes(head[..4].try_into().ok()?);
    let offset = u64::from_be_bytes(head[4..].try_into().ok()?);
    Some((song, offset, &frame[AUDIO_HEADER..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_frames_round_trip() {
        let f = audio_frame(7, 1 << 33, b"abc");
        assert_eq!(parse_audio_frame(&f), Some((7, 1 << 33, &b"abc"[..])));
        assert_eq!(parse_audio_frame(&f[..5]), None);
        assert_eq!(parse_audio_frame(&audio_frame(1, 0, b"")), Some((1, 0, &b""[..])));
    }

    #[test]
    fn actions_need_the_matching_permission() {
        assert!(Action::Suggest { text: "x".into() }.allowed(Perms::LISTENER));
        assert!(!Action::Pause.allowed(Perms::LISTENER));
        assert!(!Action::Next.allowed(Perms::LISTENER));
        assert!(Action::Seek { ms: 1 }.allowed(Perms { control: true, ..Perms::NONE }));
        assert!(!Action::Next.allowed(Perms { control: true, ..Perms::NONE }));
        assert!(Action::Previous.allowed(Perms::DJ));
        assert!(!Action::Play.allowed(Perms::NONE));
    }

    #[test]
    fn messages_have_a_stable_shape() {
        let text = serde_json::to_string(&ToHost::Request(Action::Seek { ms: 5000 })).unwrap();
        assert_eq!(serde_json::from_str::<ToHost>(&text).unwrap(), ToHost::Request(Action::Seek { ms: 5000 }));
        let text = serde_json::to_string(&Signal::Candidate { json: "{}".into() }).unwrap();
        assert!(text.contains(r#""sig":"candidate""#));
        assert!(serde_json::from_str::<ToGuest>(r#"{"t":"nope"}"#).is_err());
    }
}
