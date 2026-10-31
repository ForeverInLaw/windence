//! Reports songs heard in Windence to the listener's Spotify history.
//!
//! A device state (see [`crate::connect`]) tells Spotify what is playing now,
//! but it does not add a play to the history. The official client adds one
//! with a `RawCoreStream` event after a track ends, and librespot does not
//! send that event. Spotify does not document it, so this is off unless the
//! listener turns on "Report plays to Spotify".
//! See `docs/adr/0010-report-listens-to-spotify-history.md`.
//!
//! Only time the player really moved through a track counts. Paused time and
//! skipped parts do not, and nothing here can make up a play.

use std::{
    future::Future,
    io::Write as _,
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Result, anyhow};
use flate2::{Compression, write::GzEncoder};
use librespot::{
    core::{FileId, SpotifyUri, config},
    metadata::audio::{AudioFileFormat, AudioFiles},
    playback::player::PlayerEvent,
};
use protobuf::Message;
use sha1::{Digest as _, Sha1};

use crate::{
    connect::Identity,
    proto::{
        RawCoreStream::RawCoreStream,
        context_application_desktop::ApplicationDesktop,
        context_client_id::ClientId,
        context_device_desktop::DeviceDesktop,
        context_installation_id::InstallationId,
        context_monotonic_clock::MonotonicClock,
        context_sdk::Sdk,
        context_time::Time,
        gabito::{
            EventEnvelope, PublishEventsRequest, PublishEventsResponse,
            event_envelope::EventFragment,
        },
    },
};

/// Where the events go, on the same host as other session requests.
pub const EVENTS_ENDPOINT: &str = "/gabo-receiver-service/v3/events/";
/// The `spotify-app-version` header that goes with [`REPORTING_VERSION`].
pub const REPORTING_VERSION_HEADER: &str = "129600518";

// Spotify accepts other values here without an error, but it may then leave
// the play out of the history, so these name the desktop client they were seen
// from (librespot PR #1759).
const REPORTING_VERSION: &str = "1.2.96.518";
const REPORTING_VERSION_CODE: i64 = 129_600_518;
const REPORTING_CORE_VERSION: i64 = 6_005_400_000_002_005;
const REPORTING_SDK: &str = "0.9.4-rl-essopt-loginsend-onlinesend-bcdsend-heartbeat300.0s/30.0s-modern-payload125kB-batch100";

/// How many times one event is offered when Spotify says "try later".
const PUBLISH_ATTEMPTS: u32 = 3;

/// The biggest step between two position updates that still counts as
/// playing. Updates arrive every 250 ms, so a bigger step is a jump.
const MAX_POSITION_STEP_MS: u32 = 3_000;

/// The file formats the player tries, in its order for its default bitrate.
const PLAYER_FORMATS: [(AudioFileFormat, &str); 7] = [
    (AudioFileFormat::OGG_VORBIS_160, "Vorbis 160 kbps"),
    (AudioFileFormat::MP3_160, "MP3 160 kbps"),
    (AudioFileFormat::OGG_VORBIS_96, "Vorbis 96 kbps"),
    (AudioFileFormat::MP3_96, "MP3 96 kbps"),
    (AudioFileFormat::MP3_256, "MP3 256 kbps"),
    (AudioFileFormat::OGG_VORBIS_320, "Vorbis 320 kbps"),
    (AudioFileFormat::MP3_320, "MP3 320 kbps"),
];

/// Why a listen ended, in the words Spotify uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndReason {
    TrackDone,
    EndPlay,
}

impl EndReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::TrackDone => "trackdone",
            Self::EndPlay => "endplay",
        }
    }
}

/// The audio file the player opened for a track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioFile {
    pub id: FileId,
    pub format: &'static str,
}

/// Picks the file the player picks, because its event does not name one.
fn played_file(files: &AudioFiles) -> Option<AudioFile> {
    PLAYER_FORMATS.iter().find_map(|(format, name)| {
        files.get(format).map(|id| AudioFile {
            id: *id,
            format: name,
        })
    })
}

/// What the queue is playing, so a listen can name where it started from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayContext {
    pub track_uri: String,
    pub context_uri: Option<String>,
}

/// One finished listen, ready to report.
#[derive(Debug, Clone, PartialEq)]
pub struct Listen {
    pub uri: SpotifyUri,
    pub file: AudioFile,
    pub context_uri: Option<String>,
    pub started_at: SystemTime,
    pub ended_at: SystemTime,
    pub played_ms: u32,
    pub reason: EndReason,
}

struct Run {
    uri: SpotifyUri,
    file: AudioFile,
    context_uri: Option<String>,
    started_at: Option<SystemTime>,
    played_ms: u32,
    position_ms: Option<u32>,
}

/// Turns the player's events into finished listens.
#[derive(Default)]
pub struct Meter {
    run: Option<Run>,
}

impl Meter {
    /// Takes one player event. Returns a listen when this event ended one.
    pub fn observe(
        &mut self,
        event: &PlayerEvent,
        context: Option<&PlayContext>,
        now: SystemTime,
    ) -> Option<Listen> {
        match event {
            PlayerEvent::TrackChanged { audio_item } => {
                let ended = self.finish(EndReason::EndPlay, now);
                if let Some(file) = played_file(&audio_item.files) {
                    self.start(audio_item.track_id.clone(), file, context);
                }
                ended
            }
            PlayerEvent::Playing {
                track_id,
                position_ms,
                ..
            }
            | PlayerEvent::Seeked {
                track_id,
                position_ms,
                ..
            }
            | PlayerEvent::PositionCorrection {
                track_id,
                position_ms,
                ..
            } => {
                self.move_without_counting(track_id, Some(*position_ms));
                None
            }
            PlayerEvent::PositionChanged {
                track_id,
                position_ms,
                ..
            } => {
                self.advance(track_id, *position_ms, now);
                None
            }
            PlayerEvent::Paused {
                track_id,
                position_ms,
                ..
            } => {
                self.advance(track_id, *position_ms, now);
                self.move_without_counting(track_id, None);
                None
            }
            PlayerEvent::EndOfTrack { .. } => self.finish(EndReason::TrackDone, now),
            PlayerEvent::Loading { .. }
            | PlayerEvent::Stopped { .. }
            | PlayerEvent::Unavailable { .. } => self.finish(EndReason::EndPlay, now),
            _ => None,
        }
    }

    /// Ends the listen in progress, if any time was heard.
    pub fn finish(&mut self, reason: EndReason, now: SystemTime) -> Option<Listen> {
        let run = self.run.take()?;
        Some(Listen {
            uri: run.uri,
            file: run.file,
            context_uri: run.context_uri,
            started_at: run.started_at?,
            ended_at: now,
            played_ms: run.played_ms,
            reason,
        })
    }

    /// Starts measuring a track. Only Spotify tracks can be reported.
    fn start(&mut self, uri: SpotifyUri, file: AudioFile, context: Option<&PlayContext>) {
        if !matches!(uri, SpotifyUri::Track { .. }) {
            return;
        }
        let track_uri = uri.to_string();
        self.run = Some(Run {
            uri,
            file,
            context_uri: context
                .filter(|context| context.track_uri == track_uri)
                .and_then(|context| context.context_uri.clone()),
            started_at: None,
            played_ms: 0,
            position_ms: None,
        });
    }

    fn move_without_counting(&mut self, track: &SpotifyUri, position_ms: Option<u32>) {
        if let Some(run) = self.run.as_mut().filter(|run| run.uri == *track) {
            run.position_ms = position_ms;
        }
    }

    fn advance(&mut self, track: &SpotifyUri, position_ms: u32, now: SystemTime) {
        let Some(run) = self.run.as_mut().filter(|run| run.uri == *track) else {
            return;
        };
        let step = run
            .position_ms
            .and_then(|previous| position_ms.checked_sub(previous))
            .filter(|step| (1..=MAX_POSITION_STEP_MS).contains(step));
        if let Some(step) = step {
            run.started_at.get_or_insert(now);
            run.played_ms = run.played_ms.saturating_add(step);
        }
        run.position_ms = Some(position_ms);
    }
}

/// Why Spotify did not take an event, when it says so in a success response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rejection {
    pub reason: i32,
    /// The same bytes may be sent again.
    pub transient: bool,
}

/// Reads a response. A success status alone does not mean the event was taken.
pub fn rejection(response: &[u8]) -> Result<Option<Rejection>> {
    let response = PublishEventsResponse::parse_from_bytes(response)?;
    Ok(response.error.first().map(|error| Rejection {
        reason: error.reason,
        transient: error.transient,
    }))
}

/// Sends one event until Spotify takes it, refuses it or runs out of tries.
///
/// When Spotify says "try later", `send` goes again with the same bytes, so a
/// retry cannot count as a second play. `still_wanted` is asked before each
/// retry: a listener who turned reporting off during the wait sends nothing more.
pub async fn publish_with_retry<S, SendFut, W, WantedFut>(
    delay: Duration,
    mut send: S,
    mut still_wanted: W,
) -> Result<()>
where
    S: FnMut() -> SendFut,
    SendFut: Future<Output = Result<Vec<u8>>>,
    W: FnMut() -> WantedFut,
    WantedFut: Future<Output = bool>,
{
    let mut attempt = 1;
    loop {
        let response = send().await?;
        match rejection(&response)? {
            None => return Ok(()),
            Some(rejection) if rejection.transient && attempt < PUBLISH_ATTEMPTS => {
                tokio::time::sleep(delay * attempt).await;
                if !still_wanted().await {
                    return Ok(());
                }
                attempt += 1;
            }
            Some(rejection) => {
                return Err(anyhow!(
                    "Spotify rejected the play, reason {}",
                    rejection.reason
                ));
            }
        }
    }
}

fn timestamp_ms(time: SystemTime) -> i64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The event's body. `audio_id` is Spotify's id for the recording.
fn stream_message(listen: &Listen, audio_id: &[u8], session_id: &[u8]) -> RawCoreStream {
    let played = i32::try_from(listen.played_ms).unwrap_or(i32::MAX);
    let uri = listen.uri.to_string();
    RawCoreStream {
        playback_id: Some(rand::random::<[u8; 16]>().to_vec()),
        parent_playback_id: Some(vec![0; 16]),
        stream_id: Some(rand::random::<[u8; 16]>().to_vec()),
        media_id: Some(listen.file.id.0.to_vec()),
        audio_id: Some(hex(audio_id).into_bytes()),
        media_type: Some("audio".into()),
        video_session_id: Some(String::new()),
        play_context: Some(listen.context_uri.clone().unwrap_or_else(|| uri.clone())),
        content_uri: Some(uri),
        displayed_content_uri: Some(String::new()),
        audio_format: Some(listen.file.format.into()),
        playback_start_time: Some(timestamp_ms(listen.started_at)),
        end_timestamp: Some(timestamp_ms(listen.ended_at)),
        ms_played: Some(played),
        ms_played_nominal: Some(played),
        ms_played_overlapping: Some(0),
        ms_played_video: Some(0),
        ms_played_fullscreen: Some(0),
        ms_played_external: Some(0),
        ms_played_nominal_overlapping: Some(0),
        ms_narration_overlapping: Some(0),
        ms_trimmed: Some(0),
        ms_nominal_trimmed: Some(0),
        reason_start: Some("playbtn".into()),
        reason_end: Some(listen.reason.as_str().into()),
        source_start: Some("librespot".into()),
        source_end: Some("librespot".into()),
        referrer: Some("unknown".into()),
        provider: Some("context".into()),
        streaming_rule: Some("none".into()),
        live: Some(false),
        content_is_downloaded: Some(false),
        incognito_mode: Some(false),
        client_offline_at_stream_start: Some(false),
        social_listening: Some(false),
        is_assumed_premium: Some(true),
        connect_controller_device_id_v2: Some("local".into()),
        core_version: Some(REPORTING_CORE_VERSION),
        core_bundle: Some("full".into()),
        playback_stack: Some("boombox".into()),
        playback_stacks: vec!["boombox".into()],
        orchestration_stack: Some("context-player".into()),
        controlling_device_brand: Some("librespot".into()),
        controlling_device_model: Some("librespot".into()),
        controlling_device_type: Some("computer".into()),
        player_session_id: Some(hex(session_id)),
        ..Default::default()
    }
}

fn fragment<M: Message>(name: &str, message: &M) -> Result<EventFragment> {
    Ok(EventFragment {
        name: name.to_owned(),
        data: message.write_to_bytes()?,
        ..Default::default()
    })
}

fn decode_hex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks_exact(2)
        .filter_map(|pair| {
            let pair = std::str::from_utf8(pair).ok()?;
            u8::from_str_radix(pair, 16).ok()
        })
        .collect()
}

/// Numbers one session's events. One event keeps one number, even when its
/// bytes are sent again, so a retry cannot become a second play.
pub struct Reporter {
    session_id: [u8; 16],
    sequence_id: [u8; 20],
    sequence: i64,
    clock: Instant,
}

impl Default for Reporter {
    fn default() -> Self {
        Self {
            session_id: rand::random(),
            sequence_id: rand::random(),
            sequence: 0,
            clock: Instant::now(),
        }
    }
}

impl Reporter {
    /// Builds the gzip body of the request that reports `listen`.
    pub fn event_body(
        &mut self,
        listen: &Listen,
        identity: &Identity,
        audio_id: &[u8],
    ) -> Result<Vec<u8>> {
        self.sequence += 1;
        let request = PublishEventsRequest {
            event: vec![self.envelope(listen, identity, audio_id)?],
            ..Default::default()
        };
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&request.write_to_bytes()?)?;
        Ok(encoder.finish()?)
    }

    fn envelope(
        &self,
        listen: &Listen,
        identity: &Identity,
        audio_id: &[u8],
    ) -> Result<EventEnvelope> {
        let message = stream_message(listen, audio_id, &self.session_id);
        let event_fragment = vec![
            fragment("message", &message)?,
            fragment(
                "context_client_id",
                &ClientId {
                    value: decode_hex(&identity.client_id),
                    ..Default::default()
                },
            )?,
            fragment(
                "context_installation_id",
                &InstallationId {
                    value: Sha1::digest(identity.device_id.as_bytes())[..16].to_vec(),
                    ..Default::default()
                },
            )?,
            fragment(
                "context_application_desktop",
                &ApplicationDesktop {
                    version_string: REPORTING_VERSION.to_owned(),
                    version_code: REPORTING_VERSION_CODE,
                    session_id: self.session_id.to_vec(),
                    ..Default::default()
                },
            )?,
            fragment(
                "context_device_desktop",
                &DeviceDesktop {
                    platform_type: config::OS.to_owned(),
                    device_manufacturer: "librespot".to_owned(),
                    device_model: "librespot".to_owned(),
                    device_id: identity.device_id.clone(),
                    os_version: config::os_version(),
                    ..Default::default()
                },
            )?,
            fragment(
                "context_time",
                &Time {
                    value: timestamp_ms(listen.ended_at),
                    ..Default::default()
                },
            )?,
            fragment(
                "context_monotonic_clock",
                &MonotonicClock {
                    id: 1,
                    value: self
                        .clock
                        .elapsed()
                        .as_millis()
                        .try_into()
                        .unwrap_or(i64::MAX),
                    ..Default::default()
                },
            )?,
            fragment(
                "context_sdk",
                &Sdk {
                    version_name: REPORTING_SDK.to_owned(),
                    type_: "cpp".to_owned(),
                    ..Default::default()
                },
            )?,
            EventFragment {
                name: "context_client_context_id".to_owned(),
                ..Default::default()
            },
        ];
        Ok(EventEnvelope {
            event_name: "RawCoreStream".to_owned(),
            event_fragment,
            sequence_id: self.sequence_id.to_vec(),
            sequence_number: self.sequence,
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, collections::HashMap, io::Read as _};

    use flate2::read::GzDecoder;

    use super::*;

    const URI: &str = "spotify:track:72AZ3V52rs9NfgMNhALxln";
    const OTHER: &str = "spotify:track:5PXZP1Agwl8XCqX6NdToDG";

    fn uri(text: &str) -> SpotifyUri {
        SpotifyUri::from_uri(text).unwrap()
    }

    fn at(ms: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(ms)
    }

    fn file() -> AudioFile {
        AudioFile {
            id: FileId([7; 20]),
            format: "Vorbis 160 kbps",
        }
    }

    fn started(track: &str) -> Meter {
        let mut meter = Meter::default();
        meter.start(uri(track), file(), None);
        meter
    }

    fn playing(track: &str, position_ms: u32) -> PlayerEvent {
        PlayerEvent::Playing {
            play_request_id: 1,
            track_id: uri(track),
            position_ms,
        }
    }

    fn position(track: &str, position_ms: u32) -> PlayerEvent {
        PlayerEvent::PositionChanged {
            play_request_id: 1,
            track_id: uri(track),
            position_ms,
        }
    }

    fn end_of_track(track: &str) -> PlayerEvent {
        PlayerEvent::EndOfTrack {
            play_request_id: 1,
            track_id: uri(track),
        }
    }

    #[test]
    fn a_finished_track_reports_the_time_it_played() {
        let mut meter = started(URI);
        meter.observe(&playing(URI, 0), None, at(1_000));
        meter.observe(&position(URI, 250), None, at(1_250));
        meter.observe(&position(URI, 500), None, at(1_500));

        let listen = meter
            .observe(&end_of_track(URI), None, at(1_600))
            .expect("the track ended");

        assert_eq!(listen.played_ms, 500);
        assert_eq!(listen.reason, EndReason::TrackDone);
        assert_eq!(listen.started_at, at(1_250));
        assert_eq!(listen.ended_at, at(1_600));
    }

    #[test]
    fn paused_time_and_skipped_parts_do_not_count() {
        let mut meter = started(URI);
        meter.observe(&playing(URI, 0), None, at(0));
        meter.observe(&position(URI, 1_000), None, at(1_000));
        meter.observe(
            &PlayerEvent::Paused {
                play_request_id: 1,
                track_id: uri(URI),
                position_ms: 1_200,
            },
            None,
            at(1_200),
        );
        meter.observe(&playing(URI, 1_200), None, at(60_000));
        meter.observe(
            &PlayerEvent::Seeked {
                play_request_id: 1,
                track_id: uri(URI),
                position_ms: 90_000,
            },
            None,
            at(60_100),
        );
        meter.observe(&position(URI, 90_500), None, at(60_600));
        // A jump the player never announced is not playing time either.
        meter.observe(&position(URI, 150_000), None, at(60_850));

        let listen = meter.finish(EndReason::EndPlay, at(61_000)).unwrap();

        assert_eq!(listen.played_ms, 1_700);
    }

    #[test]
    fn a_track_that_never_played_is_not_a_listen() {
        let mut meter = started(URI);
        meter.observe(&playing(URI, 0), None, at(0));

        assert_eq!(meter.observe(&end_of_track(URI), None, at(10)), None);
    }

    #[test]
    fn skipping_to_another_track_ends_the_listen() {
        let mut meter = started(URI);
        meter.observe(&playing(URI, 0), None, at(0));
        meter.observe(&position(URI, 250), None, at(250));

        let listen = meter
            .observe(
                &PlayerEvent::Loading {
                    play_request_id: 2,
                    track_id: uri(OTHER),
                    position_ms: 0,
                },
                None,
                at(300),
            )
            .expect("the skip ended the listen");

        assert_eq!(listen.reason, EndReason::EndPlay);
        assert_eq!(listen.uri, uri(URI));
    }

    #[test]
    fn positions_of_another_track_do_not_count() {
        let mut meter = started(URI);
        meter.observe(&playing(URI, 0), None, at(0));
        meter.observe(&position(OTHER, 250), None, at(250));

        assert_eq!(meter.finish(EndReason::EndPlay, at(300)), None);
    }

    #[test]
    fn episodes_are_not_measured() {
        let meter = started("spotify:episode:72AZ3V52rs9NfgMNhALxln");

        assert!(meter.run.is_none());
    }

    #[test]
    fn a_listen_names_the_context_of_its_own_track_only() {
        let context = PlayContext {
            track_uri: URI.to_owned(),
            context_uri: Some("spotify:playlist:abc".to_owned()),
        };
        let mut matching = Meter::default();
        matching.start(uri(URI), file(), Some(&context));
        let mut stale = Meter::default();
        stale.start(uri(OTHER), file(), Some(&context));

        assert_eq!(
            matching.run.unwrap().context_uri.as_deref(),
            Some("spotify:playlist:abc")
        );
        assert_eq!(stale.run.unwrap().context_uri, None);
    }

    #[test]
    fn the_reported_file_is_the_one_the_player_prefers() {
        let files = AudioFiles(HashMap::from([
            (AudioFileFormat::OGG_VORBIS_320, FileId([3; 20])),
            (AudioFileFormat::OGG_VORBIS_160, FileId([1; 20])),
            (AudioFileFormat::MP3_96, FileId([2; 20])),
        ]));

        assert_eq!(
            played_file(&files),
            Some(AudioFile {
                id: FileId([1; 20]),
                format: "Vorbis 160 kbps",
            })
        );
    }

    fn listen() -> Listen {
        Listen {
            uri: uri(URI),
            file: file(),
            context_uri: Some("spotify:playlist:abc".to_owned()),
            started_at: at(1_700_000_000_000),
            ended_at: at(1_700_000_180_000),
            played_ms: 32_125,
            reason: EndReason::EndPlay,
        }
    }

    fn identity() -> Identity {
        Identity {
            device_id: "device".to_owned(),
            client_id: "65b70807".to_owned(),
        }
    }

    fn sent(body: &[u8]) -> PublishEventsRequest {
        let mut bytes = Vec::new();
        GzDecoder::new(body).read_to_end(&mut bytes).unwrap();
        PublishEventsRequest::parse_from_bytes(&bytes).unwrap()
    }

    #[test]
    fn the_event_carries_the_measured_time_and_the_recording() {
        let mut reporter = Reporter::default();
        let body = reporter
            .event_body(&listen(), &identity(), &[0xab; 16])
            .unwrap();

        let request = sent(&body);
        let event = &request.event[0];
        let message = RawCoreStream::parse_from_bytes(
            &event
                .event_fragment
                .iter()
                .find(|fragment| fragment.name == "message")
                .unwrap()
                .data,
        )
        .unwrap();

        assert_eq!(event.event_name, "RawCoreStream");
        assert_eq!(message.ms_played(), 32_125);
        assert_eq!(message.ms_played_nominal(), 32_125);
        assert_eq!(message.content_uri(), URI);
        assert_eq!(message.play_context(), "spotify:playlist:abc");
        assert_eq!(message.media_id(), &[7; 20]);
        assert_eq!(message.audio_id(), b"abababababababababababababababab");
        assert_eq!(message.playback_start_time(), 1_700_000_000_000);
        assert_eq!(message.end_timestamp(), 1_700_000_180_000);
        assert_eq!(message.reason_end(), "endplay");
    }

    #[test]
    fn each_event_gets_the_next_number_in_one_sequence() {
        let mut reporter = Reporter::default();
        let first = sent(
            &reporter
                .event_body(&listen(), &identity(), &[1; 16])
                .unwrap(),
        );
        let second = sent(
            &reporter
                .event_body(&listen(), &identity(), &[1; 16])
                .unwrap(),
        );

        assert_eq!(first.event[0].sequence_number, 1);
        assert_eq!(second.event[0].sequence_number, 2);
        assert_eq!(first.event[0].sequence_id, second.event[0].sequence_id);
    }

    // error[0]: index 0, transient true, reason 2.
    const TRY_LATER: [u8; 6] = [0x0a, 0x04, 0x10, 0x01, 0x18, 0x02];

    /// Runs a publish whose responses come from `responses`, and says how
    /// many times it sent and what it ended with.
    fn publish(responses: &[&[u8]], wanted: bool) -> (usize, Result<()>) {
        let sends = Cell::new(0);
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(publish_with_retry(
                Duration::ZERO,
                || {
                    let response = responses[sends.get()].to_vec();
                    sends.set(sends.get() + 1);
                    std::future::ready(Ok(response))
                },
                || std::future::ready(wanted),
            ));
        (sends.get(), result)
    }

    #[test]
    fn try_later_sends_again_until_spotify_takes_the_event() {
        let (sends, result) = publish(&[&TRY_LATER, &[]], true);

        assert_eq!(sends, 2);
        assert!(result.is_ok());
    }

    #[test]
    fn turning_reporting_off_during_the_wait_stops_the_retry() {
        let (sends, result) = publish(&[&TRY_LATER, &[]], false);

        assert_eq!(sends, 1);
        assert!(result.is_ok());
    }

    #[test]
    fn retries_stop_after_the_last_attempt() {
        let (sends, result) = publish(&[&TRY_LATER, &TRY_LATER, &TRY_LATER, &[]], true);

        assert_eq!(sends, 3);
        assert!(result.is_err());
    }

    #[test]
    fn a_success_response_can_still_reject_the_event() {
        assert_eq!(rejection(&[]).unwrap(), None);
        assert_eq!(
            rejection(&TRY_LATER).unwrap(),
            Some(Rejection {
                reason: 2,
                transient: true,
            })
        );
    }
}
