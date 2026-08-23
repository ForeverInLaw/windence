use serde::{Deserialize, Serialize};

use crate::model::Track;

/// The player-bar toggle's value. `Smart` reserves the follow-up ticket's
/// slot; nothing selects it yet.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShuffleMode {
    #[default]
    Off,
    Shuffle,
    Smart,
}

impl ShuffleMode {
    /// Whether this mode permutes the queue at all.
    pub fn shuffles(self) -> bool {
        !matches!(self, Self::Off)
    }

    /// The mode a click moves `Off` to, and `Off` back from.
    pub fn toggled(self) -> Self {
        match self {
            Self::Off => Self::Shuffle,
            Self::Shuffle | Self::Smart => Self::Off,
        }
    }
}

/// A queued track's relationship to the played context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Origin {
    /// Part of the played context; `ordinal` is its position in the
    /// context's original order.
    Context { ordinal: usize },
    /// Landed in the queue after it was built (Play next, Add to queue,
    /// Autoplay). Pinned to its queue slot: never moved by shuffling or
    /// unshuffling.
    Anchor,
}

/// The shuffle bookkeeping for one queue: the active mode, the context's
/// original order, and one [`Origin`] per queued track, aligned by index.
/// A context origin travels with its track through every reorder, so the
/// origins always say which base-order position the track now at that slot
/// came from; unshuffling sorts the upcoming ones back into ascending
/// order, which is exactly their original sequence.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ShuffleState {
    pub mode: ShuffleMode,
    /// The context's tracks in their original order, snapshotted when the
    /// context was built.
    pub context: Vec<Track>,
    pub origins: Vec<Origin>,
}

impl ShuffleState {
    /// Bookkeeping for a fresh context whose tracks start life in the given
    /// order, to be played in `mode`.
    pub fn for_context(tracks: &[Track], mode: ShuffleMode) -> Self {
        Self {
            mode,
            context: tracks.to_vec(),
            origins: (0..tracks.len())
                .map(|ordinal| Origin::Context { ordinal })
                .collect(),
        }
    }

    /// Aligns bookkeeping with an anchor track inserted at `index`.
    pub fn insert_anchor(&mut self, index: usize) {
        self.origins.insert(index, Origin::Anchor);
    }

    /// Aligns bookkeeping with an anchor track appended to the queue's end.
    pub fn push_anchor(&mut self) {
        self.origins.push(Origin::Anchor);
    }

    /// Reorders `tracks` so the queue plays in `mode`, and records the mode.
    /// The track at `playing` stays put either way: shuffling permutes only
    /// the context tracks after it, unshuffling sorts only those entries
    /// back into the original order, and anchors never move.
    pub fn set_mode(
        &mut self,
        mode: ShuffleMode,
        tracks: &mut [Track],
        playing: usize,
        rng: &mut ShuffleRng,
    ) {
        let slots = movable_slots(&self.origins, playing);
        if mode.shuffles() {
            permute_slots(tracks, &mut self.origins, &slots, rng);
        } else {
            restore_slots(tracks, &mut self.origins, &slots);
        }
        self.mode = mode;
    }

    /// Serializes the per-track origins for persistence: `None` marks an
    /// anchor, `Some(ordinal)` a context track's original position.
    pub fn ordinals(&self) -> Vec<Option<usize>> {
        self.origins
            .iter()
            .map(|origin| match origin {
                Origin::Context { ordinal } => Some(*ordinal),
                Origin::Anchor => None,
            })
            .collect()
    }

    /// Rebuilds the state persisted next to a queue of `tracks_len` tracks.
    /// Anything inconsistent — a length mismatch or an ordinal past the end
    /// of the base order — falls back to treating the queue as an unshuffled
    /// context rather than failing to restore playback. An empty base order
    /// is legitimate: a fully played-out context leaves only anchors, and
    /// the mode must survive a restart so toggle-off still works.
    pub fn restored(
        tracks_len: usize,
        mode: ShuffleMode,
        context: Vec<Track>,
        ordinals: Vec<Option<usize>>,
    ) -> Self {
        let well_formed = ordinals.len() == tracks_len
            && ordinals
                .iter()
                .all(|ordinal| ordinal.is_none_or(|ordinal| ordinal < context.len()));
        if !well_formed {
            return Self::default();
        }
        Self {
            mode,
            context,
            origins: ordinals
                .into_iter()
                .map(|ordinal| match ordinal {
                    Some(ordinal) => Origin::Context { ordinal },
                    None => Origin::Anchor,
                })
                .collect(),
        }
    }
}

/// The slots holding context tracks strictly after `playing`: exactly the
/// entries shuffling may move.
fn movable_slots(origins: &[Origin], playing: usize) -> Vec<usize> {
    origins
        .iter()
        .enumerate()
        .skip(playing.saturating_add(1))
        .filter_map(|(slot, origin)| match origin {
            Origin::Context { .. } => Some(slot),
            Origin::Anchor => None,
        })
        .collect()
}

/// Fisher–Yates over the entries at `slots`; every other entry stays put.
/// Each slot's origin travels with its track, so the origins always describe
/// which base-order position the track now sitting at that slot came from.
fn permute_slots(
    tracks: &mut [Track],
    origins: &mut [Origin],
    slots: &[usize],
    rng: &mut ShuffleRng,
) {
    for taken in (1..slots.len()).rev() {
        let pick = rng.below(taken + 1);
        let (a, b) = (slots[taken], slots[pick]);
        tracks.swap(a, b);
        origins.swap(a, b);
    }
}

/// Sorts the entries at `slots` back into the base order, leaving each
/// slot's origin ascending so the region reads as unshuffled again. Anchors
/// never sit in `slots` (see [`movable_slots`]).
fn restore_slots(tracks: &mut [Track], origins: &mut [Origin], slots: &[usize]) {
    let mut ordered = slots
        .iter()
        .map(|slot| {
            let Origin::Context { ordinal } = origins[*slot] else {
                unreachable!("movable slots only hold context tracks");
            };
            (ordinal, tracks[*slot].clone(), origins[*slot])
        })
        .collect::<Vec<_>>();
    ordered.sort_by_key(|(ordinal, _, _)| *ordinal);
    for ((_, track, origin), slot) in ordered.into_iter().zip(slots) {
        tracks[*slot] = track;
        origins[*slot] = origin;
    }
}

/// SplitMix64: a tiny, full-period generator — plenty for play ordering.
pub struct ShuffleRng(u64);

impl ShuffleRng {
    pub fn from_seed(seed: u64) -> Self {
        Self(seed)
    }

    /// Seeds from wall-clock nanos, the process id, an ASLR-assisted stack
    /// address, and a process-wide counter, so two draws can never share a
    /// stream even within the same nanosecond.
    pub fn from_entropy() -> Self {
        static CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| u64::from(since.subsec_nanos()) ^ since.as_secs());
        let pid = u64::from(std::process::id());
        let stack = &nanos as *const u64 as u64;
        let calls = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self::from_seed(
            nanos ^ (pid << 32) ^ stack.rotate_left(17) ^ calls.wrapping_mul(0x9E37_79B9_7F4A_7C15),
        )
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut mixed = self.0;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        mixed ^ (mixed >> 31)
    }

    /// A uniform draw from `0..bound`, rejection-sampled so no remainder
    /// bias. Returns 0 for an empty bound.
    pub fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        let bound = bound as u64;
        let zone = u64::MAX - u64::MAX % bound - 1;
        loop {
            let draw = self.next_u64();
            if draw <= zone {
                return (draw % bound) as usize;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Origin, ShuffleMode, ShuffleRng, ShuffleState, Track};

    fn track(id: &str) -> Track {
        Track {
            provider: crate::model::Provider::Spotify,
            source_id: id.to_owned(),
            spotify_uri: Some(format!("spotify:track:{id}")),
            isrc: None,
            title: format!("Track {id}"),
            artist: "Artist".to_owned(),
            artists: Vec::new(),
            album: "Album".to_owned(),
            album_ref: None,
            duration_ms: 180_000,
            artwork_url: None,
        }
    }

    fn tracks(ids: &[&str]) -> Vec<Track> {
        ids.iter().map(|id| track(id)).collect()
    }

    fn order(tracks: &[Track]) -> Vec<&str> {
        tracks
            .iter()
            .map(|track| track.source_id.as_str())
            .collect()
    }

    fn shuffled(ids: &[&str], playing: usize, seed: u64) -> (Vec<Track>, ShuffleState) {
        let mut queue = tracks(ids);
        let mut state = ShuffleState::for_context(&queue, ShuffleMode::Shuffle);
        state.set_mode(
            ShuffleMode::Shuffle,
            &mut queue,
            playing,
            &mut ShuffleRng::from_seed(seed),
        );
        (queue, state)
    }

    #[test]
    fn toggling_cycles_between_off_and_shuffle() {
        assert_eq!(ShuffleMode::Off.toggled(), ShuffleMode::Shuffle);
        assert_eq!(ShuffleMode::Shuffle.toggled(), ShuffleMode::Off);
        assert_eq!(ShuffleMode::Smart.toggled(), ShuffleMode::Off);
        assert!(ShuffleMode::Shuffle.shuffles());
        assert!(ShuffleMode::Smart.shuffles());
        assert!(!ShuffleMode::Off.shuffles());
    }

    #[test]
    fn shuffle_keeps_the_playing_track_fixed_and_permutes_the_rest() {
        let ids = ["a", "b", "c", "d", "e"];
        let (queue, _) = shuffled(&ids, 1, 42);

        assert_eq!(order(queue[..2].as_ref()), ["a", "b"]);
        // Every track still present exactly once: a permutation, not a deal.
        let mut tail = order(queue[2..].as_ref()).to_vec();
        tail.sort_unstable();
        assert_eq!(tail, ["c", "d", "e"]);
        // And it actually moved for this seed, so the test means something.
        assert_ne!(order(queue[2..].as_ref()), ["c", "d", "e"]);
    }

    #[test]
    fn shuffle_of_a_single_upcoming_track_changes_nothing() {
        let (queue, _) = shuffled(&["a", "b"], 0, 42);
        assert_eq!(order(&queue), ["a", "b"]);
    }

    #[test]
    fn unshuffle_restores_the_exact_pre_shuffle_vector() {
        let ids = ["a", "b", "c", "d", "e", "f"];
        let (mut queue, mut state) = shuffled(&ids, 2, 7);

        state.set_mode(
            ShuffleMode::Off,
            &mut queue,
            2,
            &mut ShuffleRng::from_seed(9),
        );

        assert_eq!(order(&queue), ids.to_vec());
    }

    #[test]
    fn unshuffle_only_touches_tracks_after_the_playing_one() {
        let ids = ["a", "b", "c", "d"];
        let (mut queue, mut state) = shuffled(&ids, 2, 3);

        state.set_mode(
            ShuffleMode::Off,
            &mut queue,
            1,
            &mut ShuffleRng::from_seed(9),
        );

        // "a" sits before the playing track, so its shuffled-time position
        // is left alone even though it precedes the restored run.
        assert_eq!(order(queue[..2].as_ref()), ["a", "b"]);
        assert_eq!(order(queue[2..].as_ref()), ["c", "d"]);
    }

    #[test]
    fn play_next_anchors_survive_shuffle_and_unshuffle_in_place() {
        let ids = ["a", "b", "c", "d", "e"];
        let (mut queue, mut state) = shuffled(&ids, 1, 42);
        // Play next lands right after the playing track...
        queue.insert(2, track("next"));
        state.insert_anchor(2);

        state.set_mode(
            ShuffleMode::Off,
            &mut queue,
            1,
            &mut ShuffleRng::from_seed(5),
        );
        // ...and neither unshuffling nor re-shuffling budges it.
        assert_eq!(queue[2], track("next"));

        state.set_mode(
            ShuffleMode::Shuffle,
            &mut queue,
            1,
            &mut ShuffleRng::from_seed(6),
        );
        assert_eq!(queue[2], track("next"));
        assert_eq!(order(queue[..3].as_ref()), ["a", "b", "next"]);
    }

    #[test]
    fn repeated_toggle_offs_are_idempotent() {
        let ids = ["a", "b", "c", "d", "e"];
        let (mut queue, mut state) = shuffled(&ids, 0, 11);

        state.set_mode(
            ShuffleMode::Off,
            &mut queue,
            0,
            &mut ShuffleRng::from_seed(1),
        );
        let once = queue.clone();
        state.set_mode(
            ShuffleMode::Off,
            &mut queue,
            0,
            &mut ShuffleRng::from_seed(2),
        );

        assert_eq!(queue, once);
        assert_eq!(order(&queue), ids.to_vec());
    }

    #[test]
    fn starting_a_context_shuffled_preserves_the_original_order_for_restore() {
        let ids = ["a", "b", "c", "d"];
        let mut queue = tracks(&ids);
        let mut state = ShuffleState::for_context(&queue, ShuffleMode::Shuffle);
        state.set_mode(
            ShuffleMode::Shuffle,
            &mut queue,
            0,
            &mut ShuffleRng::from_seed(13),
        );

        assert_ne!(order(queue[1..].as_ref()), vec!["b", "c", "d"]);
        state.set_mode(
            ShuffleMode::Off,
            &mut queue,
            0,
            &mut ShuffleRng::from_seed(14),
        );
        assert_eq!(order(&queue), ids.to_vec());
    }

    #[test]
    fn appended_autoplay_tracks_are_anchors_not_context() {
        let ids = ["a", "b", "c"];
        let (mut queue, mut state) = shuffled(&ids, 0, 21);
        queue.push(track("auto"));
        state.push_anchor();

        state.set_mode(
            ShuffleMode::Off,
            &mut queue,
            0,
            &mut ShuffleRng::from_seed(22),
        );

        assert_eq!(
            queue.last().map(|track| track.source_id.as_str()),
            Some("auto")
        );
        assert_eq!(order(queue[..3].as_ref()), ["a", "b", "c"]);
    }

    #[test]
    fn persisted_state_round_trips_through_parts() {
        let ids = ["a", "b", "c", "d"];
        let (mut queue, mut state) = shuffled(&ids, 1, 31);
        queue.push(track("next"));
        state.insert_anchor(4);

        let mut rebuilt = ShuffleState::restored(
            queue.len(),
            state.mode,
            state.context.clone(),
            state.ordinals(),
        );

        assert_eq!(rebuilt.origins, state.origins);
        assert_eq!(rebuilt.context, state.context);
        assert_eq!(rebuilt.mode, ShuffleMode::Shuffle);
        // The rebuilt state can still restore the original order.
        rebuilt.set_mode(
            ShuffleMode::Off,
            &mut queue,
            1,
            &mut ShuffleRng::from_seed(32),
        );
        assert_eq!(order(&queue), ["a", "b", "c", "d", "next"]);
    }

    #[test]
    fn malformed_persisted_state_falls_back_to_an_unshuffled_context() {
        let fallback = |len, ordinals| {
            ShuffleState::restored(len, ShuffleMode::Shuffle, tracks(&["a", "b"]), ordinals)
        };

        assert_eq!(fallback(3, vec![Some(0), Some(1)]).mode, ShuffleMode::Off);
        assert_eq!(fallback(2, vec![Some(0), Some(9)]).mode, ShuffleMode::Off);
        // An ordinal over an empty base order cannot be honoured either.
        assert_eq!(
            ShuffleState::restored(1, ShuffleMode::Shuffle, Vec::new(), vec![Some(0)]).mode,
            ShuffleMode::Off
        );
        // A consistent state is kept as-is.
        let kept = ShuffleState::restored(
            2,
            ShuffleMode::Off,
            tracks(&["a", "b"]),
            vec![Some(1), None],
        );
        assert_eq!(
            kept.origins,
            vec![Origin::Context { ordinal: 1 }, Origin::Anchor]
        );
    }

    #[test]
    fn shuffle_mode_survives_a_restart_over_anchor_only_queues() {
        // Every context track played out; autoplay anchors are all that is
        // left. Toggle-off has nothing to restore, but the toggle itself
        // must still read as on.
        let kept = ShuffleState::restored(2, ShuffleMode::Shuffle, Vec::new(), vec![None, None]);

        assert_eq!(kept.mode, ShuffleMode::Shuffle);
        assert_eq!(kept.origins, vec![Origin::Anchor, Origin::Anchor]);
    }

    #[test]
    fn seeded_draws_are_reproducible_and_within_bounds() {
        let mut first = ShuffleRng::from_seed(123);
        let mut second = ShuffleRng::from_seed(123);
        for _ in 0..100 {
            assert_eq!(first.below(7), second.below(7));
        }
        let mut rng = ShuffleRng::from_seed(5);
        for _ in 0..500 {
            assert!(rng.below(4) < 4);
        }
        assert_eq!(ShuffleRng::from_seed(1).below(0), 0);
        assert_eq!(ShuffleRng::from_seed(1).below(1), 0);
    }
}
