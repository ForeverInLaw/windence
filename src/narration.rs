//! Turning the DJ's synthesized speech into something the output device
//! can play.
//!
//! Spotify hands narration back as an MP3, which the player library
//! cannot load: its local-file path indexes a directory once at startup,
//! and these arrive mid-session. So the audio is decoded here, off the
//! output thread, and handed to the sink as plain samples.

use anyhow::{Context as _, Result, anyhow};
use librespot::playback::{NUM_CHANNELS, SAMPLE_RATE};
use symphonia::core::{
    audio::SampleBuffer, codecs::DecoderOptions, formats::FormatOptions, io::MediaSourceStream,
    meta::MetadataOptions, probe::Hint,
};

use crate::audio::NarrationClip;

/// Decodes one synthesized line into samples the sink can queue. The
/// service is asked for the playback sample rate, so a clip that comes
/// back at another one is refused rather than resampled: silence for one
/// line is a better trade than shipping a resampler for a case that has
/// never been seen.
pub(crate) fn decode(mp3: Vec<u8>) -> Result<NarrationClip> {
    let source = MediaSourceStream::new(Box::new(std::io::Cursor::new(mp3)), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("mp3");
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            source,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .context("narration audio is not readable")?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .context("narration audio carries no track")?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .context("narration audio uses an unsupported codec")?;

    let mut samples: Vec<f32> = Vec::new();
    let mut channels = 0usize;
    let mut rate = 0u32;
    let mut buffer: Option<SampleBuffer<f32>> = None;
    while let Ok(packet) = format.next_packet() {
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = decoder
            .decode(&packet)
            .context("narration audio could not be decoded")?;
        let spec = *decoded.spec();
        channels = spec.channels.count();
        rate = spec.rate;
        let buffer =
            buffer.get_or_insert_with(|| SampleBuffer::new(decoded.capacity() as u64, spec));
        buffer.copy_interleaved_ref(decoded);
        samples.extend_from_slice(buffer.samples());
    }
    if samples.is_empty() {
        return Err(anyhow!("narration audio is empty"));
    }
    Ok(NarrationClip {
        samples: to_playback_stereo(&samples, channels, rate)?,
    })
}

/// Lays decoded samples out the way the sink expects: interleaved stereo
/// at the playback sample rate. A mono line — which is what the service
/// usually returns — is widened by giving both ears the same signal.
fn to_playback_stereo(samples: &[f32], channels: usize, rate: u32) -> Result<Vec<f64>> {
    if rate != SAMPLE_RATE {
        return Err(anyhow!(
            "narration audio is {rate} Hz, but playback runs at {SAMPLE_RATE} Hz"
        ));
    }
    let wanted = usize::from(NUM_CHANNELS);
    match channels {
        0 => Err(anyhow!("narration audio has no channels")),
        1 => Ok(samples
            .iter()
            .flat_map(|sample| std::iter::repeat_n(f64::from(*sample), wanted))
            .collect()),
        found if found == wanted => Ok(samples.iter().map(|sample| f64::from(*sample)).collect()),
        found => Err(anyhow!(
            "narration audio has {found} channels, but playback takes {wanted}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use librespot::playback::SAMPLE_RATE;

    use super::to_playback_stereo;

    #[test]
    fn a_mono_line_is_widened_and_a_foreign_sample_rate_is_refused() {
        assert_eq!(
            to_playback_stereo(&[0.25, -0.5], 1, SAMPLE_RATE).unwrap(),
            [0.25, 0.25, -0.5, -0.5]
        );
        assert_eq!(
            to_playback_stereo(&[0.25, -0.5], 2, SAMPLE_RATE).unwrap(),
            [0.25, -0.5]
        );
        assert!(to_playback_stereo(&[0.25], 1, SAMPLE_RATE / 2).is_err());
        assert!(to_playback_stereo(&[], 0, SAMPLE_RATE).is_err());
        assert!(to_playback_stereo(&[0.25, -0.5, 0.75], 3, SAMPLE_RATE).is_err());
    }
}
