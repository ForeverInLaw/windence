use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::model::Track;

/// The player-bar toggle's value: off, shuffled, or shuffled with
/// recommendation injections woven in.
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

    /// The mode a click moves to: Off → Shuffle → Smart → Off, with Smart
    /// skipped where it cannot act (short lists, albums, radio).
    pub fn toggled(self, smart_available: bool) -> Self {
        match self {
            Self::Off => Self::Shuffle,
            Self::Shuffle if smart_available => Self::Smart,
            _ => Self::Off,
        }
    }
}

/// Whether Smart Shuffle may inject into a context started from here.
/// Albums expose plain shuffle only; every other list is playlist-like.
/// Radio contexts are tracked separately (`PlayQueue::radio`) and ignore
/// the toggle entirely.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextKind {
    #[default]
    Collection,
    Album,
}

impl ContextKind {
    /// Whether Smart Shuffle may act on a context of this kind at all.
    pub fn supports_smart_shuffle(self) -> bool {
        matches!(self, Self::Collection)
    }
}

/// Smart Shuffle wants a list long enough to be worth weaving through:
/// contexts shorter than this expose plain shuffle only.
pub const SMART_SHUFFLE_MIN_TRACKS: usize = 16;

/// Smart Shuffle density: one injected track per this many context tracks.
const INJECTION_EVERY: usize = 3;

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
    /// An anchor placed by Smart Shuffle: a recommendation woven into the
    /// upcoming queue. Pinned like any anchor while Smart lasts, and the
    /// first thing toggle-off removes.
    Injected,
}

impl Origin {
    /// Whether this entry belongs to the played context.
    fn is_context(self) -> bool {
        matches!(self, Self::Context { .. })
    }
}

impl Serialize for Origin {
    // Persisted as the queue snapshot's origin column: a number marks a
    // context track's original position, `null` a plain anchor, and the
    // string "injected" a Smart Shuffle injection. Older snapshots carry
    // only the first two shapes and stay readable.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Context { ordinal } => ordinal.serialize(serializer),
            Self::Anchor => serializer.serialize_none(),
            Self::Injected => serializer.serialize_str("injected"),
        }
    }
}

impl<'de> Deserialize<'de> for Origin {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct OriginVisitor;

        impl serde::de::Visitor<'_> for OriginVisitor {
            type Value = Origin;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a context ordinal, null, or \"injected\"")
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                usize::try_from(value)
                    .map(|ordinal| Origin::Context { ordinal })
                    .map_err(serde::de::Error::custom)
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                let ordinal = u64::try_from(value)
                    .map_err(|_| serde::de::Error::custom("negative context ordinal"))?;
                self.visit_u64(ordinal)
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(Origin::Anchor)
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                match value {
                    "injected" => Ok(Origin::Injected),
                    _ => Err(serde::de::Error::unknown_variant(value, &["injected"])),
                }
            }
        }

        deserializer.deserialize_any(OriginVisitor)
    }
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

    /// How many of the entries after `playing` are context tracks and how
    /// many are injections — the pair a Smart Shuffle refill balances
    /// against the target density.
    pub fn upcoming_counts(&self, playing: usize) -> (usize, usize) {
        let upcoming = self
            .origins
            .get(playing.saturating_add(1)..)
            .unwrap_or_default();
        upcoming
            .iter()
            .fold((0, 0), |(context, injected), origin| match origin {
                Origin::Context { .. } => (context + 1, injected),
                Origin::Injected => (context, injected + 1),
                Origin::Anchor => (context, injected),
            })
    }

    /// Drops every injected track queued strictly after `playing`, keeping
    /// `origins` and `tracks` aligned. Turning Smart Shuffle off calls this
    /// before restoring the original order; an injected track already
    /// playing finishes and is only removed once it passes.
    pub fn remove_upcoming_injections(&mut self, tracks: &mut Vec<Track>, playing: usize) {
        let mut slot = playing.saturating_add(1);
        while slot < self.origins.len() {
            if self.origins[slot] == Origin::Injected {
                self.origins.remove(slot);
                tracks.remove(slot);
            } else {
                slot += 1;
            }
        }
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
        origins: Vec<Origin>,
    ) -> Self {
        let well_formed = origins.len() == tracks_len
            && origins.iter().all(|origin| match origin {
                Origin::Context { ordinal } => *ordinal < context.len(),
                _ => true,
            });
        if !well_formed {
            return Self::default();
        }
        Self {
            mode,
            context,
            origins,
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
        .filter_map(|(slot, origin)| origin.is_context().then_some(slot))
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

/// How many injections the density asks for over an upcoming region of
/// `context_count` context tracks.
pub fn injection_target(context_count: usize) -> usize {
    context_count / INJECTION_EVERY
}

/// Where `wanted` injected tracks belong in an upcoming region, as ascending
/// slot indexes after which each should be inserted. Indexes are relative to
/// the region's start — the entry right after the playing track is 0 — so
/// callers translate by the playing index themselves and apply them in
/// reverse so earlier inserts never shift later slots.
///
/// The walk counts context tracks in play order and lands an injection
/// after every [`INJECTION_EVERY`]th one, sliding past runs of anchors so a
/// Play-next pick stays glued to the track the listener heard last, and
/// never stacking onto an injection already sitting in a gap.
pub fn injection_slots(origins: &[Origin], wanted: usize) -> Vec<usize> {
    let mut slots = Vec::new();
    let mut run = 0;
    let mut index = 0;
    while index < origins.len() && slots.len() < wanted {
        if !origins[index].is_context() {
            index += 1;
            continue;
        }
        run += 1;
        if run < INJECTION_EVERY {
            index += 1;
            continue;
        }
        run = 0;
        let mut landing = index + 1;
        while matches!(origins.get(landing), Some(Origin::Anchor)) {
            landing += 1;
        }
        if !matches!(origins.get(landing), Some(Origin::Injected)) {
            slots.push(landing - 1);
        }
        index = landing;
    }
    slots
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
    use super::{
        ContextKind, Origin, ShuffleMode, ShuffleRng, ShuffleState, Track, injection_slots,
    };

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

    /// Marks every context entry as an `Origin::Context` in order.
    fn context_origins(len: usize) -> Vec<Origin> {
        (0..len)
            .map(|ordinal| Origin::Context { ordinal })
            .collect()
    }

    #[test]
    fn toggling_walks_off_shuffle_then_smart_then_back_to_off() {
        assert_eq!(ShuffleMode::Off.toggled(true), ShuffleMode::Shuffle);
        assert_eq!(ShuffleMode::Shuffle.toggled(true), ShuffleMode::Smart);
        assert_eq!(ShuffleMode::Smart.toggled(true), ShuffleMode::Off);
        // Where Smart cannot act the cycle skips straight back to Off.
        assert_eq!(ShuffleMode::Off.toggled(false), ShuffleMode::Shuffle);
        assert_eq!(ShuffleMode::Shuffle.toggled(false), ShuffleMode::Off);
        assert!(ShuffleMode::Shuffle.shuffles());
        assert!(ShuffleMode::Smart.shuffles());
        assert!(!ShuffleMode::Off.shuffles());
    }

    #[test]
    fn collection_contexts_admit_smart_shuffle_and_albums_do_not() {
        assert!(ContextKind::Collection.supports_smart_shuffle());
        assert!(!ContextKind::Album.supports_smart_shuffle());
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
    fn injection_slots_land_after_every_third_context_track() {
        assert_eq!(injection_slots(&context_origins(9), 3), vec![2, 5, 8]);
        // Density holds for partial trailing groups too.
        assert_eq!(injection_slots(&context_origins(7), 5), vec![2, 5]);
        // Fewer context tracks than one full group admits no injection.
        assert!(injection_slots(&context_origins(2), 1).is_empty());
        assert!(injection_slots(&context_origins(0), 1).is_empty());
    }

    #[test]
    fn injection_slots_slide_past_anchor_runs_so_user_pins_keep_their_place() {
        // Two Play-next pins sit mid-region: the gap due after the third
        // context track opens up beyond the pin run instead of splitting it.
        let origins = vec![
            Origin::Context { ordinal: 0 },
            Origin::Context { ordinal: 1 },
            Origin::Context { ordinal: 2 },
            Origin::Anchor,
            Origin::Anchor,
            Origin::Context { ordinal: 3 },
            Origin::Context { ordinal: 4 },
            Origin::Context { ordinal: 5 },
        ];
        assert_eq!(injection_slots(&origins, 2), vec![4, 7]);
    }

    #[test]
    fn injection_slots_never_stack_onto_an_existing_injection() {
        let origins = vec![
            Origin::Context { ordinal: 0 },
            Origin::Context { ordinal: 1 },
            Origin::Context { ordinal: 2 },
            Origin::Injected,
            Origin::Context { ordinal: 3 },
            Origin::Context { ordinal: 4 },
            Origin::Context { ordinal: 5 },
        ];
        // The first group already carries an injection; only the second
        // group's gap is free.
        assert_eq!(injection_slots(&origins, 2), vec![6]);
    }

    #[test]
    fn upcoming_counts_separate_context_tracks_from_injections() {
        let (mut queue, mut state) = shuffled(&["a", "b", "c", "d"], 0, 3);
        queue.push(track("next"));
        state.push_anchor();
        queue.push(track("smart"));
        state.origins.push(Origin::Injected);

        assert_eq!(state.upcoming_counts(0), (3, 1));
        assert_eq!(state.upcoming_counts(4), (0, 1));
        assert_eq!(state.upcoming_counts(5), (0, 0));
    }

    #[test]
    fn leaving_smart_removes_upcoming_injections_then_unshuffles_cleanly() {
        let ids = ["a", "b", "c", "d", "e", "f", "g"];
        let (mut queue, mut state) = shuffled(&ids, 0, 42);
        // Two recommendations woven in, as the engine would place them.
        queue.insert(3, track("x1"));
        state.origins.insert(3, Origin::Injected);
        queue.insert(7, track("x2"));
        state.origins.insert(7, Origin::Injected);

        state.remove_upcoming_injections(&mut queue, 0);
        state.set_mode(
            ShuffleMode::Off,
            &mut queue,
            0,
            &mut ShuffleRng::from_seed(43),
        );

        assert_eq!(order(&queue), ids.to_vec());
        assert_eq!(state.origins.len(), queue.len());
        assert!(!state.origins.contains(&Origin::Injected));
    }

    #[test]
    fn an_injected_track_already_playing_finishes_when_smart_turns_off() {
        let ids = ["a", "b", "c", "d", "e", "f", "g"];
        let (mut queue, mut state) = shuffled(&ids, 0, 42);
        // The listener has moved onto the first injection; another waits
        // further down the queue.
        queue.insert(1, track("x1"));
        state.origins.insert(1, Origin::Injected);
        queue.insert(4, track("x2"));
        state.origins.insert(4, Origin::Injected);
        let playing = 1;

        state.remove_upcoming_injections(&mut queue, playing);
        state.set_mode(
            ShuffleMode::Off,
            &mut queue,
            playing,
            &mut ShuffleRng::from_seed(44),
        );

        // The playing injection finishes; everything after it is the
        // untouched context in its original order.
        assert_eq!(order(&queue), vec!["a", "x1", "b", "c", "d", "e", "f", "g"]);
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
            state.origins.clone(),
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
        let fallback = |len, origins| {
            ShuffleState::restored(len, ShuffleMode::Shuffle, tracks(&["a", "b"]), origins)
        };

        assert_eq!(
            fallback(3, vec![Origin::Context { ordinal: 0 }, Origin::Anchor]).mode,
            ShuffleMode::Off
        );
        assert_eq!(
            fallback(
                2,
                vec![
                    Origin::Context { ordinal: 0 },
                    Origin::Context { ordinal: 9 }
                ]
            )
            .mode,
            ShuffleMode::Off
        );
        // An ordinal over an empty base order cannot be honoured either.
        assert_eq!(
            ShuffleState::restored(
                1,
                ShuffleMode::Shuffle,
                Vec::new(),
                vec![Origin::Context { ordinal: 0 }]
            )
            .mode,
            ShuffleMode::Off
        );
        // A consistent state is kept as-is.
        let kept = ShuffleState::restored(
            2,
            ShuffleMode::Off,
            tracks(&["a", "b"]),
            vec![Origin::Context { ordinal: 1 }, Origin::Anchor],
        );
        assert_eq!(
            kept.origins,
            vec![Origin::Context { ordinal: 1 }, Origin::Anchor]
        );
    }

    #[test]
    fn origin_serialization_reads_back_legacy_snapshots_and_injections() {
        let origins = vec![
            Origin::Context { ordinal: 0 },
            Origin::Anchor,
            Origin::Injected,
        ];
        let json = serde_json::to_string(&origins).unwrap();
        assert_eq!(json, r#"[0,null,"injected"]"#);
        assert_eq!(serde_json::from_str::<Vec<Origin>>(&json).unwrap(), origins);
        // Snapshots written before Smart Shuffle existed hold plain
        // Option-style arrays; they must keep restoring.
        assert_eq!(
            serde_json::from_str::<Vec<Origin>>("[null,2]").unwrap(),
            vec![Origin::Anchor, Origin::Context { ordinal: 2 }]
        );
        assert!(serde_json::from_str::<Vec<Origin>>("[\"surprise\"]").is_err());
    }

    #[test]
    fn shuffle_mode_survives_a_restart_over_anchor_only_queues() {
        // Every context track played out; autoplay anchors are all that is
        // left. Toggle-off has nothing to restore, but the toggle itself
        // must still read as on.
        let kept = ShuffleState::restored(
            2,
            ShuffleMode::Shuffle,
            Vec::new(),
            vec![Origin::Anchor, Origin::Anchor],
        );

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
