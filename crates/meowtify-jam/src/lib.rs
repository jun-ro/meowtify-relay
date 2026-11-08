//! Jam sessions: several people listen together, in sync with a host.
//!
//! This crate holds what the signaling relay and the app share. The relay only introduces the
//! host and the guests to each other and tracks who is in the room; the audio and the playback
//! state travel over direct connections between them.
//!
//! - [`wire`]: the JSON frames between a client and the relay.
//! - [`proto`]: what host and guests say over their direct connection, and who may ask for what.
//! - [`follower`]: what a guest does to follow the host.
//! - [`link`]: the `meowtify://jam/...` link a host shares.
//! - [`sync`]: the clock estimate and drift rules that keep a guest in step.
//! - [`room`]: the room logic. It does no I/O and reads no clock, so a Cloudflare Durable Object
//!   and a native server run the same code.

pub mod follower;
pub mod link;
pub mod proto;
pub mod room;
pub mod sync;
pub mod wire;

pub use link::JamLink;
pub use room::{Config, ConnId, Env, Out, Room};
pub use wire::{room_code, ClientMsg, ErrorCode, LeftReason, Member, MemberId, Resume, ServerMsg, Target};
