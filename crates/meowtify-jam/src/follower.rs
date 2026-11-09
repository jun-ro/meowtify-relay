//! What a guest does to follow the host. Pure decisions: the host's state, the guest's clock
//! and player go in, and a list of steps comes out. The app carries the steps out.

use std::collections::{HashMap, HashSet};

use crate::proto::{HostState, SongId};
use crate::sync::{correction, expected_pos, ClockSync};

/// Songs after the current one that are fetched ahead, so a skip does not wait for a download.
const PREFETCH: usize = 2;
/// After the guest starts or seeks the player, it is left alone this long to settle.
const SETTLE_MS: u64 = 1_500;

/// What the guest's player is doing, as the app reads it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Local {
    /// The file that is loaded.
    pub path: Option<String>,
    pub pos_ms: u32,
    pub playing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Ask the host for this song's audio.
    Want(SongId),
    /// Load the file and start it at `pos_ms`, playing or paused.
    Play { song: SongId, path: String, pos_ms: u32, playing: bool },
    Seek(u32),
    Pause,
    Resume,
    /// Nothing should be heard now.
    Stop,
}

#[derive(Debug, Default)]
pub struct Follower {
    state: Option<HostState>,
    clock: ClockSync,
    /// Songs whose audio is complete on disk.
    files: HashMap<SongId, String>,
    wanted: HashSet<SongId>,
    /// The song the guest's player was told to play.
    loaded: Option<SongId>,
    settle_until: u64,
}

impl Follower {
    pub fn new() -> Follower {
        Follower::default()
    }

    pub fn clock_mut(&mut self) -> &mut ClockSync {
        &mut self.clock
    }

    pub fn state(&self) -> Option<&HostState> {
        self.state.as_ref()
    }

    /// The host sent its state. A state older than the one held is ignored.
    pub fn on_state(&mut self, state: HostState, now_ms: u64, local: &Local) -> Vec<Step> {
        if self.state.as_ref().is_some_and(|s| state.rev <= s.rev) {
            return Vec::new();
        }
        self.state = Some(state);
        self.plan(now_ms, local)
    }

    /// All of a song's audio has arrived at `path`.
    pub fn on_file(&mut self, song: SongId, path: String, now_ms: u64, local: &Local) -> Vec<Step> {
        self.files.insert(song, path);
        self.wanted.remove(&song);
        self.plan(now_ms, local)
    }

    /// A song could not be had (the host said so, or the transfer failed). It may be asked for again.
    pub fn on_missing(&mut self, song: SongId) {
        self.wanted.remove(&song);
    }

    /// The file of a song was deleted (the cache made room).
    pub fn forget(&mut self, song: SongId) {
        self.files.remove(&song);
        if self.loaded == Some(song) {
            self.loaded = None;
        }
    }

    /// Call about once a second: corrects drift and notices a player that stopped on its own.
    pub fn on_tick(&mut self, now_ms: u64, local: &Local) -> Vec<Step> {
        self.plan(now_ms, local)
    }

    fn plan(&mut self, now_ms: u64, local: &Local) -> Vec<Step> {
        let Some(state) = self.state.clone() else { return Vec::new() };
        let mut steps = Vec::new();
        let Some(song) = &state.song else {
            if self.loaded.take().is_some() {
                steps.push(Step::Stop);
            }
            return steps;
        };

        for s in std::iter::once(song).chain(state.queue.iter().take(PREFETCH)) {
            if !self.files.contains_key(&s.id) && self.wanted.insert(s.id) {
                steps.push(Step::Want(s.id));
            }
        }

        let Some(path) = self.files.get(&song.id).cloned() else {
            // The new song is not here yet: silence is better than the old song.
            if self.loaded.take().is_some() {
                steps.push(Step::Stop);
            }
            return steps;
        };
        let expected = expected_pos(&state, self.clock.host_time(now_ms));
        let unloaded = self.loaded != Some(song.id) || local.path.as_deref() != Some(path.as_str());
        if unloaded {
            self.loaded = Some(song.id);
            self.settle_until = now_ms + SETTLE_MS;
            steps.push(Step::Play { song: song.id, path, pos_ms: expected, playing: state.playing });
            return steps;
        }
        if now_ms < self.settle_until {
            return steps;
        }
        if state.playing != local.playing {
            steps.push(if state.playing { Step::Resume } else { Step::Pause });
            self.settle_until = now_ms + SETTLE_MS;
        }
        if let Some(to) = correction(local.pos_ms, expected) {
            steps.push(Step::Seek(to));
            self.settle_until = now_ms + SETTLE_MS;
        }
        steps
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::SongRef;

    fn song(id: SongId) -> SongRef {
        SongRef { id, title: format!("song {id}"), artist: "a".into(), duration_ms: 200_000, key: None, by: None }
    }

    fn state(rev: u64, current: Option<SongId>, playing: bool, pos_ms: u32, at_ms: u64, queue: &[SongId]) -> HostState {
        HostState { rev, song: current.map(song), playing, pos_ms, at_ms, queue: queue.iter().map(|&i| song(i)).collect() }
    }

    fn local(path: &str, pos_ms: u32, playing: bool) -> Local {
        Local { path: Some(path.into()), pos_ms, playing }
    }

    #[test]
    fn joining_mid_song_fetches_then_plays_from_where_the_host_is() {
        let mut f = Follower::new();
        let steps = f.on_state(state(1, Some(1), true, 30_000, 1_000, &[2, 3, 4]), 5_000, &Local::default());
        assert_eq!(steps, vec![Step::Want(1), Step::Want(2), Step::Want(3)], "the current song and two ahead");

        // The file arrives 3 s later; the host has moved on 3 s by then.
        let steps = f.on_file(1, "/c/1".into(), 8_000, &Local::default());
        assert_eq!(steps, vec![Step::Play { song: 1, path: "/c/1".into(), pos_ms: 37_000, playing: true }]);
    }

    #[test]
    fn a_late_or_repeated_state_is_ignored() {
        let mut f = Follower::new();
        f.on_state(state(5, Some(1), true, 0, 0, &[]), 0, &Local::default());
        assert!(f.on_state(state(5, Some(2), true, 0, 0, &[]), 0, &Local::default()).is_empty());
        assert!(f.on_state(state(4, Some(2), true, 0, 0, &[]), 0, &Local::default()).is_empty());
        assert_eq!(f.state().unwrap().song.as_ref().unwrap().id, 1);
    }

    fn playing_follower() -> Follower {
        let mut f = Follower::new();
        f.on_state(state(1, Some(1), true, 0, 0, &[]), 0, &Local::default());
        f.on_file(1, "/c/1".into(), 0, &Local::default());
        f
    }

    #[test]
    fn drift_is_corrected_after_the_player_settles_and_not_before() {
        let mut f = playing_follower();
        // 10 s later the host is at 10 s; the guest is at 9 s. Inside the settle time: left alone.
        assert!(f.on_tick(1_000, &local("/c/1", 900, true)).is_empty());
        assert!(f.on_tick(10_000, &local("/c/1", 9_000, true)).contains(&Step::Seek(10_000)));
        // Right after a seek it is left alone again.
        assert!(f.on_tick(10_500, &local("/c/1", 9_100, true)).is_empty());
        // In step: nothing to do.
        assert!(f.on_tick(20_000, &local("/c/1", 20_100, true)).is_empty());
    }

    #[test]
    fn pause_and_resume_follow_the_host() {
        let mut f = playing_follower();
        let steps = f.on_state(state(2, Some(1), false, 12_000, 10_000, &[]), 10_050, &local("/c/1", 12_000, true));
        assert_eq!(steps, vec![Step::Pause]);
        let steps = f.on_state(state(3, Some(1), true, 12_000, 20_000, &[]), 20_050, &local("/c/1", 12_000, false));
        assert_eq!(steps, vec![Step::Resume]);
    }

    #[test]
    fn a_new_song_is_silent_until_its_file_is_here_then_plays() {
        let mut f = playing_follower();
        let steps = f.on_state(state(2, Some(2), true, 0, 10_000, &[3]), 10_000, &local("/c/1", 10_000, true));
        assert_eq!(steps, vec![Step::Want(2), Step::Want(3), Step::Stop]);
        let steps = f.on_file(2, "/c/2".into(), 11_000, &Local::default());
        assert_eq!(steps, vec![Step::Play { song: 2, path: "/c/2".into(), pos_ms: 1_000, playing: true }]);
    }

    #[test]
    fn a_prefetched_song_plays_at_once() {
        let mut f = Follower::new();
        f.on_state(state(1, Some(1), true, 0, 0, &[2]), 0, &Local::default());
        f.on_file(1, "/c/1".into(), 0, &Local::default());
        f.on_file(2, "/c/2".into(), 0, &Local::default());
        let steps = f.on_state(state(2, Some(2), true, 0, 5_000, &[]), 5_000, &local("/c/1", 5_000, true));
        assert_eq!(steps, vec![Step::Play { song: 2, path: "/c/2".into(), pos_ms: 0, playing: true }]);
    }

    #[test]
    fn nothing_playing_stops_the_player_once() {
        let mut f = playing_follower();
        f.on_tick(5_000, &local("/c/1", 5_000, true));
        assert_eq!(f.on_state(state(2, None, false, 0, 0, &[]), 6_000, &local("/c/1", 6_000, true)), vec![Step::Stop]);
        assert!(f.on_tick(7_000, &Local::default()).is_empty());
    }

    #[test]
    fn a_missing_song_can_be_asked_for_again_and_a_forgotten_file_is_fetched_again() {
        let mut f = Follower::new();
        assert_eq!(f.on_state(state(1, Some(1), true, 0, 0, &[]), 0, &Local::default()), vec![Step::Want(1)]);
        assert!(f.on_tick(1_000, &Local::default()).is_empty(), "already asked");
        f.on_missing(1);
        assert_eq!(f.on_tick(2_000, &Local::default()), vec![Step::Want(1)]);
        f.on_file(1, "/c/1".into(), 3_000, &Local::default());
        f.forget(1);
        assert_eq!(f.on_tick(4_000, &Local::default()), vec![Step::Want(1)]);
    }

    #[test]
    fn a_player_that_dropped_the_song_gets_it_again() {
        let mut f = playing_follower();
        let steps = f.on_tick(9_000, &Local::default());
        assert!(matches!(steps.as_slice(), [Step::Play { song: 1, pos_ms: 9_000, .. }]));
    }
}
