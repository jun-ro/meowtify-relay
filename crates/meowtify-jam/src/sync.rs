//! Keeping a guest's playback in step with the host's.

use crate::proto::HostState;

/// A guest seeks only when it is this far from the host. Smaller corrections would be heard.
pub const DRIFT_LIMIT_MS: u32 = 300;
/// Clock samples kept. The middle one is used, so a slow round trip does not shift the offset.
const SAMPLES: usize = 7;

/// The guest's estimate of the host's clock, from ping round trips.
#[derive(Debug, Clone, Default)]
pub struct ClockSync {
    /// `host - local`, in milliseconds, one per answered ping.
    offsets: Vec<i64>,
}

impl ClockSync {
    /// A ping sent at `sent_ms` (local clock) came back at `got_ms` (local), with the host's
    /// clock reading `host_ms` when it answered. The host answered about halfway.
    pub fn sample(&mut self, sent_ms: u64, got_ms: u64, host_ms: u64) {
        if got_ms < sent_ms {
            return;
        }
        let mid = sent_ms + (got_ms - sent_ms) / 2;
        self.offsets.push(host_ms as i64 - mid as i64);
        if self.offsets.len() > SAMPLES {
            self.offsets.remove(0);
        }
    }

    pub fn is_ready(&self) -> bool {
        !self.offsets.is_empty()
    }

    /// The host's clock for a local reading, or the local one until a ping is answered.
    pub fn host_time(&self, local_ms: u64) -> u64 {
        if self.offsets.is_empty() {
            return local_ms;
        }
        let mut sorted = self.offsets.clone();
        sorted.sort_unstable();
        let offset = sorted[sorted.len() / 2];
        (local_ms as i64 + offset).max(0) as u64
    }
}

/// Where the host is in the song at `host_now_ms` on its clock.
pub fn expected_pos(state: &HostState, host_now_ms: u64) -> u32 {
    let Some(song) = &state.song else { return 0 };
    if !state.playing {
        return state.pos_ms;
    }
    let elapsed = host_now_ms.saturating_sub(state.at_ms).min(u32::MAX as u64) as u32;
    let pos = state.pos_ms.saturating_add(elapsed);
    if song.duration_ms > 0 { pos.min(song.duration_ms) } else { pos }
}

/// The position to seek to, when `local_ms` is too far from `expected_ms`.
pub fn correction(local_ms: u32, expected_ms: u32) -> Option<u32> {
    (local_ms.abs_diff(expected_ms) > DRIFT_LIMIT_MS).then_some(expected_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::SongRef;

    fn state(playing: bool, pos_ms: u32, at_ms: u64, duration_ms: u32) -> HostState {
        HostState {
            song: Some(SongRef { id: 1, title: "t".into(), artist: "a".into(), duration_ms, key: None, by: None }),
            playing,
            pos_ms,
            at_ms,
            ..HostState::default()
        }
    }

    #[test]
    fn the_host_clock_is_estimated_from_pings() {
        let mut c = ClockSync::default();
        assert_eq!(c.host_time(1_000), 1_000, "until a ping is answered the local clock stands in");
        // The host's clock is 5 s ahead. Round trips of 40 ms.
        c.sample(1_000, 1_040, 6_020);
        assert_eq!(c.host_time(2_000), 7_000);
        // One slow round trip (a spike) does not move the middle value.
        c.sample(2_000, 2_900, 7_450);
        c.sample(3_000, 3_040, 8_020);
        c.sample(4_000, 4_040, 9_020);
        assert_eq!(c.host_time(5_000), 10_000);
        c.sample(10, 5, 0); // an impossible sample is ignored
        assert!(c.is_ready());
    }

    #[test]
    fn the_expected_position_moves_only_while_playing_and_stops_at_the_end() {
        assert_eq!(expected_pos(&state(true, 10_000, 1_000, 200_000), 3_500), 12_500);
        assert_eq!(expected_pos(&state(false, 10_000, 1_000, 200_000), 3_500), 10_000);
        assert_eq!(expected_pos(&state(true, 10_000, 1_000, 11_000), 9_000), 11_000);
        assert_eq!(expected_pos(&state(true, 10_000, 5_000, 200_000), 4_000), 10_000, "a clock a little behind");
        assert_eq!(expected_pos(&HostState::default(), 99), 0);
    }

    #[test]
    fn small_drift_is_left_alone() {
        assert_eq!(correction(10_000, 10_250), None);
        assert_eq!(correction(10_000, 9_800), None);
        assert_eq!(correction(10_000, 10_400), Some(10_400));
        assert_eq!(correction(10_400, 10_000), Some(10_000));
    }
}
