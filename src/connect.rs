//! What Cadence tells Spotify it is playing.
//!
//! Every Spotify client PUTs a device state to
//! `/connect-state/v1/devices/<device_id>`: who the device is, and what its
//! player is doing. That report is how a play reaches the account's history
//! and how other clients learn to move a playlist up their own library
//! list. Without it Spotify does not know this app exists, and a play here
//! is invisible everywhere else.
//!
//! Cadence reports and takes no orders. The device says it is hidden and
//! cannot be played to, so it stays out of the Connect picker on the phone:
//! tapping a device is a promise to start playing, and Cadence cannot keep
//! that promise yet.
//!
//! Everything here is pure. See [`crate::playback`] for the request that
//! carries it.

use librespot::{
    core::version,
    protocol::{
        connect::{Capabilities, Device, DeviceInfo, MemberType, PutStateReason, PutStateRequest},
        devices::DeviceType,
        player::{PlayOrigin, PlayerState, ProvidedTrack},
    },
};
use protobuf::{EnumOrUnknown, MessageField};

/// The name the account's device list shows.
pub const DEVICE_NAME: &str = "Cadence";

/// How the account knows this install: the device Spotify issued for the
/// playback session, and the app it was issued to.
pub struct Identity {
    pub device_id: String,
    pub client_id: String,
}

/// What the player is doing, as far as Spotify needs to know.
///
/// A report is only ever about a track Spotify itself can name, so the uri
/// is the Spotify one and never a local id.
pub struct Report {
    pub track_uri: String,
    /// What the track was started from — a playlist, an album, the liked
    /// songs. Missing when the queue was built rather than opened.
    pub context_uri: Option<String>,
    /// Names this run of this track. It stays the same while the track
    /// plays, so several reports about one track are one play and not many.
    pub playback_id: String,
    pub position_ms: u32,
    pub duration_ms: u32,
    pub playing: bool,
}

/// The state to PUT.
///
/// `now_ms` is the wall clock the report is stamped with. The server
/// extrapolates the position from it, which is why a plain tick of the
/// seeker needs no report of its own.
pub fn state_request(
    identity: &Identity,
    report: Option<&Report>,
    reason: PutStateReason,
    now_ms: i64,
) -> PutStateRequest {
    PutStateRequest {
        device: MessageField::some(Device {
            device_info: MessageField::some(device_info(identity)),
            player_state: MessageField::some(player_state(report, now_ms)),
            ..Default::default()
        }),
        member_type: EnumOrUnknown::new(MemberType::CONNECT_STATE),
        put_state_reason: EnumOrUnknown::new(reason),
        // Active means this device holds the account's playback right now,
        // which is true exactly while it is playing.
        is_active: report.is_some_and(|report| report.playing),
        client_side_timestamp: now_ms.try_into().unwrap_or_default(),
        ..Default::default()
    }
}

/// The device as the account sees it: a reporter, not a speaker.
fn device_info(identity: &Identity) -> DeviceInfo {
    DeviceInfo {
        can_play: false,
        name: DEVICE_NAME.to_owned(),
        device_id: identity.device_id.clone(),
        client_id: identity.client_id.clone(),
        device_type: EnumOrUnknown::new(DeviceType::COMPUTER),
        device_software_version: version::SEMVER.to_owned(),
        spirc_version: version::SPOTIFY_SPIRC_VERSION.to_owned(),
        capabilities: MessageField::some(Capabilities {
            // Every one of these is a way in for a command Cadence has no
            // answer to, so every one of them is off.
            can_be_player: false,
            is_controllable: false,
            supports_transfer_command: false,
            supports_command_request: false,
            connect_disabled: true,
            hidden: true,
            disable_volume: true,
            supported_types: vec!["audio/track".to_owned()],
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// The player half. With nothing playing this is an empty state rather than
/// a made-up one: the device is announcing itself, not a track.
fn player_state(report: Option<&Report>, now_ms: i64) -> PlayerState {
    let mut state = PlayerState {
        timestamp: now_ms,
        ..Default::default()
    };
    let Some(report) = report else {
        return state;
    };
    state.context_uri = report.context_uri.clone().unwrap_or_default();
    state.track = MessageField::some(ProvidedTrack {
        uri: report.track_uri.clone(),
        provider: "context".to_owned(),
        ..Default::default()
    });
    state.play_origin = MessageField::some(PlayOrigin {
        feature_identifier: "cadence".to_owned(),
        feature_version: env!("CARGO_PKG_VERSION").to_owned(),
        ..Default::default()
    });
    state.playback_id = report.playback_id.clone();
    state.position_as_of_timestamp = report.position_ms.into();
    state.duration = report.duration_ms.into();
    state.is_playing = report.playing;
    state.is_paused = !report.playing;
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Identity {
        Identity {
            device_id: "device-1".to_owned(),
            client_id: "client-1".to_owned(),
        }
    }

    fn report(playing: bool) -> Report {
        Report {
            track_uri: "spotify:track:aaa".to_owned(),
            context_uri: Some("spotify:playlist:bbb".to_owned()),
            playback_id: "play-1".to_owned(),
            position_ms: 42_000,
            duration_ms: 200_000,
            playing,
        }
    }

    #[test]
    fn a_playing_report_names_the_track_its_context_and_where_the_seeker_is() {
        let request = state_request(
            &identity(),
            Some(&report(true)),
            PutStateReason::PLAYER_STATE_CHANGED,
            1_700_000_000_000,
        );

        let player = &request.device.player_state;
        assert_eq!(player.track.uri, "spotify:track:aaa");
        assert_eq!(player.context_uri, "spotify:playlist:bbb");
        assert_eq!(player.position_as_of_timestamp, 42_000);
        assert_eq!(player.duration, 200_000);
        assert_eq!(player.timestamp, 1_700_000_000_000);
        assert_eq!(player.playback_id, "play-1");
        assert!(player.is_playing);
        assert!(!player.is_paused);
        // Playing is what makes this the account's active device.
        assert!(request.is_active);
        assert_eq!(request.client_side_timestamp, 1_700_000_000_000);
    }

    #[test]
    fn a_paused_report_says_paused_and_gives_the_account_back() {
        let request = state_request(
            &identity(),
            Some(&report(false)),
            PutStateReason::PLAYER_STATE_CHANGED,
            1_700_000_000_000,
        );

        let player = &request.device.player_state;
        assert!(!player.is_playing);
        assert!(player.is_paused);
        // The track is still named: paused is a state, not an absence.
        assert_eq!(player.track.uri, "spotify:track:aaa");
        assert!(!request.is_active);
    }

    #[test]
    fn a_device_with_nothing_playing_announces_only_itself() {
        let request = state_request(
            &identity(),
            None,
            PutStateReason::NEW_DEVICE,
            1_700_000_000_000,
        );

        let player = &request.device.player_state;
        assert!(player.track.is_none());
        assert!(player.context_uri.is_empty());
        assert!(!player.is_playing);
        assert!(!request.is_active);

        let device = &request.device.device_info;
        assert_eq!(device.name, DEVICE_NAME);
        assert_eq!(device.device_id, "device-1");
        assert_eq!(device.client_id, "client-1");
    }

    #[test]
    fn the_device_offers_nothing_that_could_be_commanded() {
        let request = state_request(&identity(), None, PutStateReason::NEW_DEVICE, 0);
        let device = &request.device.device_info;
        let capabilities = &device.capabilities;

        assert!(!device.can_play);
        assert!(!capabilities.can_be_player);
        assert!(!capabilities.is_controllable);
        assert!(!capabilities.supports_transfer_command);
        assert!(!capabilities.supports_command_request);
        // Both of these keep the device out of the picker on the phone.
        assert!(capabilities.connect_disabled);
        assert!(capabilities.hidden);
    }
}
