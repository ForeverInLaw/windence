//! Audio output: the one place sound leaves the process. Songs arrive from
//! librespot's player as packets. The DJ's spoken lines arrive through an
//! inbox and are queued on the same stream ahead of the song they
//! introduce, so the song follows the voice without a gap.
//!
//! The device runs at whatever rate and format it prefers; playback audio
//! is resampled to match. Samples cross to the device callback through a
//! ring buffer. A stream that fails (the device was unplugged) or a default
//! device that moved (headphones connected) is reopened on the new default,
//! within a time limit, so a device that never returns surfaces an error
//! instead of hanging playback.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use async_channel as async_chan;
use cpal::{
    DeviceId, FromSample, SampleFormat, SizedSample, Stream, StreamConfig, SupportedStreamConfig,
    SupportedStreamConfigRange,
    traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _},
};
use librespot::playback::{
    NUM_CHANNELS, SAMPLE_RATE,
    audio_backend::{Sink, SinkError, SinkResult},
    convert::Converter,
    decoder::AudioPacket,
    mixer::VolumeGetter,
};
use ringbuf::{
    HeapCons, HeapProd, HeapRb,
    traits::{Consumer as _, Producer as _, Split as _},
};
use rubato::{Fft, FixedSync, Resampler as _, audioadapter_buffers::direct::InterleavedSlice};

/// How much audio waits between the sink and the device callback.
const QUEUE_TARGET: Duration = Duration::from_millis(150);
const MAX_QUEUE_BYTES: usize = 64 * 1024 * 1024;
const RESAMPLER_CHUNK_FRAMES: usize = 256;
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);
/// How long a lost device is waited for before the failure reaches the player.
const RECOVERY_TIMEOUT: Duration = Duration::from_secs(5);
const RECOVERY_RETRY_DELAY: Duration = Duration::from_millis(100);
/// How often the default device is compared with the one playing.
const DEVICE_POLL: Duration = Duration::from_secs(1);
/// How much of a spoken line is handed over at a time. Short enough that
/// moving the volume slider is heard almost at once, because each piece is
/// turned down as it goes; long enough that the device does not run dry
/// between pieces.
const NARRATION_CHUNK: Duration = Duration::from_millis(120);
/// Sample formats the sink can feed, most preferred first.
const OUTPUT_FORMATS: [SampleFormat; 6] = [
    SampleFormat::F32,
    SampleFormat::I16,
    SampleFormat::I32,
    SampleFormat::U16,
    SampleFormat::U32,
    SampleFormat::F64,
];

/// Something to say before the next song: already decoded, interleaved,
/// and at the playback sample rate, because the audio thread must not be
/// held up decoding.
pub struct NarrationClip {
    pub samples: Vec<f64>,
}

/// How many samples of interleaved audio `span` covers.
fn samples_in(span: Duration) -> usize {
    let frames = span.as_secs_f64() * f64::from(SAMPLE_RATE);
    (frames as usize).max(1) * usize::from(NUM_CHANNELS)
}

/// Opens the default output device. Spoken lines share the stream with
/// the songs so they can be queued ahead of them, which is why the sink
/// takes their inbox.
///
/// Song samples arrive already turned down by the player's own mixer.
/// Spoken lines do not pass through it, so `song_volume` is that same
/// mixer's getter and the sink applies it to them itself. `interrupted`
/// is how the listener cuts a line short.
pub fn open(
    narration: async_chan::Receiver<NarrationClip>,
    song_volume: Box<dyn VolumeGetter + Send>,
    interrupted: Arc<AtomicBool>,
) -> Box<dyn Sink> {
    Box::new(CpalSink {
        output: Output::open(),
        voice: Voice {
            inbox: narration,
            song_volume,
            interrupted,
        },
    })
}

struct CpalSink {
    output: Output,
    voice: Voice,
}

/// The DJ's side of the sink: lines waiting to be said, and what it takes
/// to say them at the right volume and stop when asked.
struct Voice {
    inbox: async_chan::Receiver<NarrationClip>,
    /// Reads the volume songs are already playing at. Asked once per piece
    /// of a line, so the voice follows the slider while it is speaking.
    song_volume: Box<dyn VolumeGetter + Send>,
    /// Set when the listener skips, pauses or stops. A line is queued a
    /// piece at a time, so this is what lets the rest of it be dropped
    /// instead of played out first.
    interrupted: Arc<AtomicBool>,
}

impl Voice {
    /// Says every line waiting in the inbox, one after the other.
    fn speak_pending(&self, feed: &mut Feed, converter: &mut Converter) -> SinkResult<()> {
        while let Ok(clip) = self.inbox.try_recv() {
            self.speak(&clip, feed, converter)?;
        }
        Ok(())
    }

    /// Speaks a line a piece at a time, and does not return until the line
    /// is done, so the writer cannot slip song audio in front of the voice.
    ///
    /// Each piece is turned down by the volume as it stands right then,
    /// which is what lets the slider reach a line already speaking. The
    /// queue holds only a piece or two, so a line the listener interrupts
    /// stops being heard almost at once instead of playing itself out.
    fn speak(
        &self,
        clip: &NarrationClip,
        feed: &mut Feed,
        converter: &mut Converter,
    ) -> SinkResult<()> {
        for piece in clip.samples.chunks(samples_in(NARRATION_CHUNK)) {
            if self.interrupted.load(Ordering::Relaxed) {
                return Ok(());
            }
            let attenuation = self.song_volume.attenuation_factor();
            let turned_down: Vec<f64> = piece.iter().map(|sample| sample * attenuation).collect();
            feed.write(converter.f64_to_f32(&turned_down))?;
        }
        Ok(())
    }
}

/// Hands one song packet to the device. Anything the DJ has to say goes in
/// first, so the song queues up behind the voice and follows it without a
/// gap.
fn deliver(
    voice: &Voice,
    feed: &mut Feed,
    packet: AudioPacket,
    converter: &mut Converter,
) -> SinkResult<()> {
    voice.speak_pending(feed, converter)?;
    let samples = packet
        .samples()
        .map_err(|error| SinkError::OnWrite(error.to_string()))?;
    feed.write(converter.f64_to_f32(samples))
}

fn wait_until_cleared(
    clear_requested: &AtomicBool,
    failed: &AtomicBool,
    timeout: Duration,
) -> SinkResult<()> {
    let deadline = Instant::now() + timeout;
    while clear_requested.load(Ordering::Acquire) {
        if failed.load(Ordering::Acquire) {
            return Err(SinkError::StateChange(
                "audio output stream failed".to_owned(),
            ));
        }
        if Instant::now() >= deadline {
            return Err(SinkError::StateChange(
                "audio output did not start consuming samples".to_owned(),
            ));
        }
        thread::sleep(Duration::from_millis(2));
    }
    if failed.load(Ordering::Acquire) {
        Err(SinkError::StateChange(
            "audio output stream failed".to_owned(),
        ))
    } else {
        Ok(())
    }
}

/// A write that failed because the stream itself failed is answered by
/// reopening the output, not by handing the player the error.
fn recover_failed_write(
    result: SinkResult<()>,
    stream_failed: bool,
    recover: impl FnOnce() -> SinkResult<()>,
) -> SinkResult<()> {
    if result.is_err() && stream_failed {
        recover()
    } else {
        result
    }
}

/// Keeps trying `attempt` while it says the failure may pass, up to the
/// recovery time limit. Each attempt learns the deadline so it can bound
/// its own waiting.
fn retry_recovery(mut attempt: impl FnMut(Instant) -> (SinkResult<()>, bool)) -> SinkResult<()> {
    let deadline = Instant::now() + RECOVERY_TIMEOUT;
    loop {
        let (result, retryable) = attempt(deadline);
        match result {
            Ok(()) => return Ok(()),
            Err(error) if retryable => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(error);
                }
                thread::sleep(RECOVERY_RETRY_DELAY.min(remaining));
                if Instant::now() >= deadline {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
    }
}

fn select_supported_config(
    configs: &[SupportedStreamConfigRange],
    channels: Option<u16>,
    mut resolve: impl FnMut(SupportedStreamConfigRange) -> Option<SupportedStreamConfig>,
) -> Option<SupportedStreamConfig> {
    OUTPUT_FORMATS.into_iter().find_map(|format| {
        configs
            .iter()
            .filter(|config| {
                config.sample_format() == format
                    && channels.is_none_or(|channels| config.channels() == channels)
            })
            .find_map(|config| resolve(*config))
    })
}

fn select_nearest_config(configs: &[SupportedStreamConfigRange]) -> Option<SupportedStreamConfig> {
    OUTPUT_FORMATS.into_iter().find_map(|format| {
        configs
            .iter()
            .filter(|config| config.sample_format() == format)
            .map(|config| {
                let sample_rate =
                    SAMPLE_RATE.clamp(config.min_sample_rate(), config.max_sample_rate());
                (sample_rate.abs_diff(SAMPLE_RATE), *config, sample_rate)
            })
            .min_by_key(|(distance, _, _)| *distance)
            .and_then(|(_, config, sample_rate)| config.try_with_sample_rate(sample_rate))
    })
}

/// Picks what the device is opened with. Stereo at the playback rate comes
/// first, so nothing is resampled or remapped; then stereo at a standard
/// rate; then the device's own default; then any channel count; and last
/// whatever rate is nearest to playback.
fn select_output_config(
    configs: &[SupportedStreamConfigRange],
    default: Option<SupportedStreamConfig>,
) -> Option<SupportedStreamConfig> {
    let stereo = Some(u16::from(NUM_CHANNELS));
    select_supported_config(configs, stereo, |config| {
        config.try_with_sample_rate(SAMPLE_RATE)
    })
    .or_else(|| {
        select_supported_config(configs, stereo, |config| {
            config.try_with_standard_sample_rate()
        })
    })
    .or_else(|| {
        default.filter(|config| {
            config.channels() > 0 && OUTPUT_FORMATS.contains(&config.sample_format())
        })
    })
    .or_else(|| {
        select_supported_config(configs, None, |config| {
            config.try_with_sample_rate(SAMPLE_RATE)
        })
    })
    .or_else(|| {
        select_supported_config(configs, None, |config| {
            config.try_with_standard_sample_rate()
        })
    })
    .or_else(|| select_nearest_config(configs))
}

fn queue_capacity(sample_rate: u32, channels: u16) -> Result<usize, String> {
    let samples = u128::from(sample_rate) * QUEUE_TARGET.as_millis() / 1_000 * u128::from(channels);
    let samples = usize::try_from(samples)
        .map_err(|_| "audio output queue size exceeds platform range".to_owned())?;
    if samples > MAX_QUEUE_BYTES / std::mem::size_of::<f32>() {
        return Err("audio output queue would exceed 64 MiB".to_owned());
    }
    Ok(samples)
}

/// Lays stereo frames out for a device with another channel count: mono
/// gets the two mixed, wider layouts get the pair followed by silence.
fn map_output_channels(samples: Vec<f32>, output_channels: u16) -> Vec<f32> {
    match output_channels {
        0 => Vec::new(),
        2 => samples,
        1 => samples
            .chunks_exact(2)
            .map(|frame| (frame[0] + frame[1]) * 0.5)
            .collect(),
        channels => {
            let channels = usize::from(channels);
            let mut mapped = Vec::with_capacity(samples.len() / 2 * channels);
            for frame in samples.chunks_exact(2) {
                mapped.extend_from_slice(frame);
                mapped.resize(mapped.len() + channels - 2, 0.0);
            }
            mapped
        }
    }
}

fn playback_buffer(capacity: usize) -> (BufferWriter, BufferReader) {
    let (producer, consumer) = HeapRb::new(capacity).split();
    (BufferWriter(producer), BufferReader(consumer))
}

struct BufferWriter(HeapProd<f32>);

impl BufferWriter {
    fn write(&mut self, samples: &[f32]) -> usize {
        self.0.push_slice(samples)
    }

    /// Queues every sample, waiting for the callback to make room. Gives up
    /// when the stream fails, is stopped, or stops draining the queue.
    fn write_all(
        &mut self,
        samples: &[f32],
        playing: &AtomicBool,
        failed: &AtomicBool,
    ) -> SinkResult<()> {
        let deadline = Instant::now() + WRITE_TIMEOUT;
        let mut written = 0;
        while written < samples.len() {
            if failed.load(Ordering::Acquire) {
                return Err(SinkError::OnWrite("audio output stream failed".to_owned()));
            }
            if !playing.load(Ordering::Acquire) {
                return Err(SinkError::StateChange(
                    "audio output is not playing".to_owned(),
                ));
            }
            written += self.write(&samples[written..]);
            if written < samples.len() {
                if Instant::now() >= deadline {
                    return Err(SinkError::OnWrite(
                        "audio output stopped consuming samples".to_owned(),
                    ));
                }
                thread::sleep(Duration::from_millis(2));
            }
        }
        Ok(())
    }
}

struct BufferReader(HeapCons<f32>);

impl BufferReader {
    fn clear(&mut self) {
        self.0.clear();
    }

    /// Fills the device's buffer, with silence once the queue runs dry.
    fn fill_converted<T>(&mut self, output: &mut [T])
    where
        T: FromSample<f32> + SizedSample,
    {
        for sample in output {
            *sample = T::from_sample(self.0.try_pop().unwrap_or(0.0));
        }
    }
}

/// Brings playback audio to the device's sample rate.
enum OutputConverter {
    Passthrough,
    Resampling {
        resampler: Box<Fft<f32>>,
        /// Input that did not yet fill a whole resampler chunk.
        pending: Vec<f32>,
    },
}

impl OutputConverter {
    fn new(input_rate: u32, output_rate: u32) -> Result<Self, String> {
        if input_rate == output_rate {
            return Ok(Self::Passthrough);
        }
        let input_rate = usize::try_from(input_rate)
            .map_err(|_| "input sample rate exceeds platform range".to_owned())?;
        let output_rate = usize::try_from(output_rate)
            .map_err(|_| "output sample rate exceeds platform range".to_owned())?;
        let resampler = Fft::new(
            input_rate,
            output_rate,
            RESAMPLER_CHUNK_FRAMES,
            usize::from(NUM_CHANNELS),
            FixedSync::Input,
        )
        .map_err(|error| format!("could not configure audio resampling: {error}"))?;
        Ok(Self::Resampling {
            resampler: Box::new(resampler),
            pending: Vec::new(),
        })
    }

    fn convert(&mut self, samples: Vec<f32>) -> Result<Vec<f32>, String> {
        let Self::Resampling { resampler, pending } = self else {
            return Ok(samples);
        };

        pending.extend_from_slice(&samples);
        let frames_per_chunk = resampler.input_frames_next();
        let channels = usize::from(NUM_CHANNELS);
        let samples_per_chunk = frames_per_chunk
            .checked_mul(channels)
            .ok_or_else(|| "audio resampling chunk size overflowed".to_owned())?;
        let complete_chunks = pending.len() / samples_per_chunk;
        let mut converted = Vec::new();
        for chunk in pending[..complete_chunks * samples_per_chunk].chunks_exact(samples_per_chunk)
        {
            let input = InterleavedSlice::new(chunk, channels, frames_per_chunk)
                .map_err(|error| format!("could not prepare audio for resampling: {error}"))?;
            let output = resampler
                .process(&input, None)
                .map_err(|error| format!("could not resample audio: {error}"))?;
            converted.extend(output.take_data());
        }
        pending.drain(..complete_chunks * samples_per_chunk);
        Ok(converted)
    }

    fn reset(&mut self) {
        if let Self::Resampling {
            resampler, pending, ..
        } = self
        {
            pending.clear();
            resampler.reset();
        }
    }
}

/// The device as the sink sees it: open, failed to open, or between the
/// two while a reopen is under way.
enum Output {
    Ready(Box<Playback>),
    Failed(AudioOpenError),
    Reopening,
}

struct Playback {
    stream: Stream,
    /// The device the stream was opened on; `None` when it would not say.
    device_id: Option<DeviceId>,
    device_checked: Instant,
    feed: Feed,
}

/// The way from playback samples to the device callback: resample, lay
/// out the channels, and queue.
struct Feed {
    writer: BufferWriter,
    converter: OutputConverter,
    output_channels: u16,
    clear_requested: Arc<AtomicBool>,
    playing: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
}

impl Feed {
    fn write(&mut self, samples: Vec<f32>) -> SinkResult<()> {
        let samples = self
            .converter
            .convert(samples)
            .map_err(SinkError::OnWrite)?;
        let samples = map_output_channels(samples, self.output_channels);
        self.writer.write_all(&samples, &self.playing, &self.failed)
    }
}

enum AudioOpenError {
    Connection(String),
    InvalidFormat(String),
}

impl AudioOpenError {
    fn sink_error(&self) -> SinkError {
        match self {
            Self::Connection(error) => SinkError::ConnectionRefused(error.clone()),
            Self::InvalidFormat(error) => SinkError::InvalidParams(error.clone()),
        }
    }
}

impl Output {
    fn open() -> Self {
        match Playback::open() {
            Ok(playback) => Self::Ready(Box::new(playback)),
            Err(error) => Self::Failed(error),
        }
    }

    fn playback_mut(&mut self) -> SinkResult<&mut Playback> {
        match self {
            Self::Ready(playback) => Ok(playback),
            Self::Failed(error) => Err(error.sink_error()),
            Self::Reopening => unreachable!(),
        }
    }

    fn stream_failed(&self) -> bool {
        matches!(self, Self::Ready(playback) if playback.feed.failed.load(Ordering::Acquire))
    }

    /// Whether another attempt at opening may go better: a device that
    /// went away can come back, and the default can change.
    fn recovery_retryable(&self) -> bool {
        match self {
            Self::Ready(playback) => playback.feed.failed.load(Ordering::Acquire),
            Self::Failed(_) => true,
            Self::Reopening => false,
        }
    }

    fn recover_output(&mut self) -> SinkResult<()> {
        retry_recovery(|deadline| {
            let result = self.start_before(deadline);
            (result, self.recovery_retryable())
        })
    }

    fn recover_stream_failure(&mut self) -> SinkResult<()> {
        log::warn!("audio output stream failed; reopening the default output");
        self.recover_output()
    }

    /// Starts the stream, reopening it first if it is unusable, and waits
    /// until the callback has dropped whatever was queued before.
    fn start_before(&mut self, deadline: Instant) -> SinkResult<()> {
        let should_reopen = match self {
            Self::Ready(playback) => playback.feed.failed.load(Ordering::Acquire),
            Self::Failed(_) => true,
            Self::Reopening => unreachable!(),
        };
        if should_reopen {
            let previous = std::mem::replace(self, Self::Reopening);
            drop(previous);
            *self = Self::open();
        }

        let playback = self.playback_mut()?;
        let feed = &mut playback.feed;
        feed.clear_requested.store(true, Ordering::Release);
        feed.converter.reset();
        if let Err(error) = playback.stream.play() {
            feed.failed.store(true, Ordering::Release);
            return Err(SinkError::StateChange(error.to_string()));
        }
        let timeout = deadline.saturating_duration_since(Instant::now());
        if let Err(error) = wait_until_cleared(&feed.clear_requested, &feed.failed, timeout) {
            feed.failed.store(true, Ordering::Release);
            let _ = playback.stream.pause();
            return Err(error);
        }
        feed.playing.store(true, Ordering::Release);
        Ok(())
    }
}

fn default_output_device_id() -> Option<DeviceId> {
    cpal::default_host()
        .default_output_device()
        .and_then(|device| device.id().ok())
}

impl Playback {
    fn open() -> Result<Self, AudioOpenError> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or_else(|| {
                AudioOpenError::Connection("no default audio output is available".to_owned())
            })?;
        let supported_configs = device
            .supported_output_configs()
            .map_err(|error| {
                AudioOpenError::Connection(format!("could not query audio output formats: {error}"))
            })?
            .filter(|config| {
                config.channels() > 0 && OUTPUT_FORMATS.contains(&config.sample_format())
            })
            .collect::<Vec<_>>();
        let output_config =
            select_output_config(&supported_configs, device.default_output_config().ok())
                .ok_or_else(|| {
                    AudioOpenError::InvalidFormat(
                        "default audio output does not support a compatible format".to_owned(),
                    )
                })?;
        let sample_format = output_config.sample_format();
        let config = output_config.config();
        let capacity = queue_capacity(config.sample_rate, config.channels)
            .map_err(AudioOpenError::InvalidFormat)?;
        let converter = OutputConverter::new(SAMPLE_RATE, config.sample_rate)
            .map_err(AudioOpenError::InvalidFormat)?;
        let (writer, reader) = playback_buffer(capacity);
        let clear_requested = Arc::new(AtomicBool::new(true));
        let playing = Arc::new(AtomicBool::new(false));
        let failed = Arc::new(AtomicBool::new(false));
        let stream = match sample_format {
            SampleFormat::F32 => build_output_stream::<f32>(
                &device,
                config,
                reader,
                Arc::clone(&clear_requested),
                Arc::clone(&failed),
            ),
            SampleFormat::I16 => build_output_stream::<i16>(
                &device,
                config,
                reader,
                Arc::clone(&clear_requested),
                Arc::clone(&failed),
            ),
            SampleFormat::U16 => build_output_stream::<u16>(
                &device,
                config,
                reader,
                Arc::clone(&clear_requested),
                Arc::clone(&failed),
            ),
            SampleFormat::I32 => build_output_stream::<i32>(
                &device,
                config,
                reader,
                Arc::clone(&clear_requested),
                Arc::clone(&failed),
            ),
            SampleFormat::U32 => build_output_stream::<u32>(
                &device,
                config,
                reader,
                Arc::clone(&clear_requested),
                Arc::clone(&failed),
            ),
            SampleFormat::F64 => build_output_stream::<f64>(
                &device,
                config,
                reader,
                Arc::clone(&clear_requested),
                Arc::clone(&failed),
            ),
            _ => unreachable!("sample format was filtered above"),
        }?;
        log::info!(
            "audio output opened on {device} at {} Hz, {} channels, {sample_format}",
            config.sample_rate,
            config.channels
        );

        Ok(Self {
            stream,
            device_id: device.id().ok(),
            device_checked: Instant::now(),
            feed: Feed {
                writer,
                converter,
                output_channels: config.channels,
                clear_requested,
                playing,
                failed,
            },
        })
    }

    /// Whether the default output is no longer the device playing. A
    /// stream stays on the device it opened on, so a new default (Bluetooth
    /// headphones connecting) is only followed by reopening. Checked once
    /// a second unless `now`; a device that gave no id cannot be compared,
    /// so it is never reported as moved.
    // ponytail: polling; register an IMMNotificationClient if a second of
    // delay after a device switch ever matters.
    fn default_device_moved(&mut self, now: bool) -> bool {
        if self.device_id.is_none() || (!now && self.device_checked.elapsed() < DEVICE_POLL) {
            return false;
        }
        self.device_checked = Instant::now();
        default_output_device_id() != self.device_id
    }
}

fn build_output_stream<T>(
    device: &cpal::Device,
    config: StreamConfig,
    mut reader: BufferReader,
    clear_requested: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
) -> Result<Stream, AudioOpenError>
where
    T: FromSample<f32> + SizedSample,
{
    device
        .build_output_stream(
            config,
            move |output: &mut [T], _| {
                if clear_requested.load(Ordering::Acquire) {
                    reader.clear();
                    clear_requested.store(false, Ordering::Release);
                }
                reader.fill_converted(output);
            },
            move |error| {
                log::warn!("audio output stream failed: {error}");
                failed.store(true, Ordering::Release);
            },
            None,
        )
        .map_err(|error| {
            AudioOpenError::Connection(format!("could not open audio output: {error}"))
        })
}

impl CpalSink {
    /// Marks the stream as finished with when the default device is no
    /// longer the one playing, so the next write reopens on the new one.
    fn follow_default_device(&mut self, now: bool) {
        if let Output::Ready(playback) = &mut self.output
            && !playback.feed.failed.load(Ordering::Acquire)
            && playback.default_device_moved(now)
        {
            log::info!("default audio output changed; moving playback to it");
            playback.feed.failed.store(true, Ordering::Release);
        }
    }

    fn write_packet(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let playback = self.output.playback_mut()?;
        deliver(&self.voice, &mut playback.feed, packet, converter)
    }
}

impl Sink for CpalSink {
    fn start(&mut self) -> SinkResult<()> {
        self.follow_default_device(true);
        self.output.recover_output()
    }

    fn stop(&mut self) -> SinkResult<()> {
        // Whatever of a line is still queued goes with the song it
        // belonged to. A line already handed over but not yet queued is
        // not withdrawn — it is spoken before the next song instead.
        if let Output::Ready(playback) = &mut self.output {
            let feed = &playback.feed;
            feed.playing.store(false, Ordering::Release);
            feed.clear_requested.store(true, Ordering::Release);
            if let Err(error) = playback.stream.pause() {
                feed.failed.store(true, Ordering::Release);
                log::warn!("could not pause audio output: {error}");
            }
        }
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        self.follow_default_device(false);
        if self.output.stream_failed() {
            self.output.recover_stream_failure()?;
        }

        let result = self.write_packet(packet, converter);
        let stream_failed = self.output.stream_failed();
        recover_failed_write(result, stream_failed, || {
            self.output.recover_stream_failure()
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU32;

    use cpal::{SupportedBufferSize, SupportedStreamConfig, SupportedStreamConfigRange};

    use super::*;

    /// Reads a volume that changes each time it is asked, so a test can
    /// tell which piece of a line saw which setting.
    struct SteppingVolume {
        asked: AtomicU32,
    }

    impl VolumeGetter for SteppingVolume {
        fn attenuation_factor(&self) -> f64 {
            match self.asked.fetch_add(1, Ordering::Relaxed) {
                0 => 1.0,
                _ => 0.5,
            }
        }
    }

    fn voice(interrupted: bool) -> (async_chan::Sender<NarrationClip>, Voice) {
        let (sender, inbox) = async_chan::unbounded();
        let voice = Voice {
            inbox,
            song_volume: Box::new(SteppingVolume {
                asked: AtomicU32::new(0),
            }),
            interrupted: Arc::new(AtomicBool::new(interrupted)),
        };
        (sender, voice)
    }

    /// A feed over a buffer with no device behind it, already playing.
    fn feed(capacity: usize) -> (Feed, BufferReader) {
        let (writer, reader) = playback_buffer(capacity);
        let feed = Feed {
            writer,
            converter: OutputConverter::new(SAMPLE_RATE, SAMPLE_RATE).unwrap(),
            output_channels: 2,
            clear_requested: Arc::new(AtomicBool::new(false)),
            playing: Arc::new(AtomicBool::new(true)),
            failed: Arc::new(AtomicBool::new(false)),
        };
        (feed, reader)
    }

    fn queued(reader: &mut BufferReader, count: usize) -> Vec<f32> {
        let mut output = vec![f32::NAN; count];
        reader.fill_converted(&mut output);
        output
    }

    #[test]
    fn a_spoken_line_is_queued_before_the_song_it_introduces() {
        let (sender, voice) = voice(false);
        let (mut feed, mut reader) = feed(8);
        sender
            .try_send(NarrationClip {
                samples: vec![1.0, 1.0],
            })
            .unwrap();

        deliver(
            &voice,
            &mut feed,
            AudioPacket::Samples(vec![0.25, 0.25]),
            &mut Converter::new(None),
        )
        .unwrap();

        assert_eq!(queued(&mut reader, 4), [1.0, 1.0, 0.25, 0.25]);
    }

    #[test]
    fn each_piece_of_a_line_takes_the_volume_as_it_stands() {
        let (sender, voice) = voice(false);
        let piece = samples_in(NARRATION_CHUNK);
        let (mut feed, mut reader) = feed(piece * 2);
        sender
            .try_send(NarrationClip {
                samples: vec![1.0; piece * 2],
            })
            .unwrap();

        voice
            .speak_pending(&mut feed, &mut Converter::new(None))
            .unwrap();

        let heard = queued(&mut reader, piece * 2);
        assert_eq!(heard[0], 1.0);
        assert_eq!(heard[piece], 0.5);
    }

    #[test]
    fn an_interrupted_line_is_dropped_and_the_song_goes_ahead() {
        let (sender, voice) = voice(true);
        let (mut feed, mut reader) = feed(8);
        sender
            .try_send(NarrationClip {
                samples: vec![1.0, 1.0],
            })
            .unwrap();

        deliver(
            &voice,
            &mut feed,
            AudioPacket::Samples(vec![0.25, 0.25]),
            &mut Converter::new(None),
        )
        .unwrap();

        assert_eq!(queued(&mut reader, 4), [0.25, 0.25, 0.0, 0.0]);
    }

    #[test]
    fn a_piece_of_a_line_covers_its_span_on_every_channel() {
        let channels = usize::from(NUM_CHANNELS);
        let a_second = SAMPLE_RATE as usize * channels;

        assert_eq!(samples_in(Duration::from_secs(1)), a_second);
        assert_eq!(samples_in(Duration::from_millis(500)), a_second / 2);
        // Never zero: chunking a line by nothing would not terminate.
        assert_eq!(samples_in(Duration::ZERO), channels);
    }

    #[test]
    fn playback_buffer_fills_underruns_with_silence() {
        let (mut writer, mut reader) = playback_buffer(4);
        writer.write(&[0.25, -0.5]);

        assert_eq!(queued(&mut reader, 4), [0.25, -0.5, 0.0, 0.0]);
    }

    #[test]
    fn clearing_playback_buffer_discards_queued_audio() {
        let (mut writer, mut reader) = playback_buffer(4);
        writer.write(&[0.25, -0.5]);

        reader.clear();

        assert_eq!(queued(&mut reader, 2), [0.0, 0.0]);
    }

    #[test]
    fn playback_buffer_holds_150_milliseconds() {
        assert_eq!(queue_capacity(44_100, 2).unwrap(), 13_230);
    }

    #[test]
    fn playback_buffer_rejects_unbounded_device_formats() {
        assert!(queue_capacity(u32::MAX, u16::MAX).is_err());
    }

    #[test]
    fn start_waits_for_the_callback_to_clear_stale_audio() {
        let clear_requested = Arc::new(AtomicBool::new(true));
        let failed = AtomicBool::new(false);
        let callback_clear = Arc::clone(&clear_requested);
        let callback = thread::spawn(move || {
            thread::sleep(Duration::from_millis(1));
            callback_clear.store(false, Ordering::Release);
        });

        wait_until_cleared(&clear_requested, &failed, Duration::from_secs(1)).unwrap();
        callback.join().unwrap();
    }

    #[test]
    fn startup_fails_if_the_stream_failed_while_clearing() {
        let clear_requested = AtomicBool::new(false);
        let failed = AtomicBool::new(true);

        assert!(matches!(
            wait_until_cleared(&clear_requested, &failed, Duration::from_secs(1)),
            Err(SinkError::StateChange(_))
        ));
    }

    #[test]
    fn queued_writes_fail_immediately_after_stream_failure() {
        let (mut writer, _reader) = playback_buffer(1);
        writer.write(&[0.25]);
        let playing = AtomicBool::new(true);
        let failed = AtomicBool::new(true);

        let result = writer.write_all(&[0.5], &playing, &failed);

        assert!(matches!(result, Err(SinkError::OnWrite(_))));
    }

    #[test]
    fn recovered_stream_failures_do_not_escape_to_the_player() {
        let write_result = Err(SinkError::OnWrite("stream failed".to_owned()));
        let mut recovered = false;

        let result = recover_failed_write(write_result, true, || {
            recovered = true;
            Ok(())
        });

        assert!(result.is_ok());
        assert!(recovered);
    }

    #[test]
    fn transient_device_handoffs_are_retried() {
        let mut attempts = 0;

        let result = retry_recovery(|_| {
            attempts += 1;
            if attempts == 1 {
                (
                    Err(SinkError::ConnectionRefused(
                        "device unavailable".to_owned(),
                    )),
                    true,
                )
            } else {
                (Ok(()), false)
            }
        });

        assert!(result.is_ok());
        assert_eq!(attempts, 2);
    }

    #[test]
    fn sample_rate_conversion_has_bounded_non_accumulating_delay() {
        let mut converter = OutputConverter::new(44_100, 48_000).unwrap();
        let input = vec![0.25; 44_100 * 2];

        let first_output = converter.convert(input.clone()).unwrap();
        let second_output = converter.convert(input).unwrap();

        assert_eq!(first_output.len() / 2, 47_680);
        assert_eq!(second_output.len() / 2, 48_000);
    }

    #[test]
    fn stereo_audio_is_mapped_to_the_device_channel_count() {
        let input = vec![0.5, -0.25, 1.0, -0.5];

        assert_eq!(map_output_channels(input.clone(), 1), vec![0.125, 0.25]);
        assert_eq!(
            map_output_channels(input, 4),
            vec![0.5, -0.25, 0.0, 0.0, 1.0, -0.5, 0.0, 0.0]
        );
    }

    fn stereo(rate: u32, format: SampleFormat) -> SupportedStreamConfigRange {
        SupportedStreamConfigRange::new(2, rate, rate, SupportedBufferSize::Unknown, format)
    }

    #[test]
    fn output_config_prefers_the_playback_rate_in_the_first_supported_format() {
        let configs = [
            stereo(48_000, SampleFormat::F32),
            stereo(44_100, SampleFormat::I16),
            stereo(44_100, SampleFormat::F32),
        ];

        let selected = select_output_config(&configs, None).unwrap();

        assert_eq!(selected.sample_rate(), 44_100);
        assert_eq!(selected.sample_format(), SampleFormat::F32);
    }

    #[test]
    fn output_config_takes_a_standard_rate_when_the_playback_rate_is_missing() {
        let configs = [stereo(48_000, SampleFormat::I16)];

        let selected = select_output_config(&configs, None).unwrap();

        assert_eq!(selected.sample_rate(), 48_000);
        assert_eq!(selected.sample_format(), SampleFormat::I16);
    }

    #[test]
    fn output_config_falls_back_to_a_nonstandard_sample_rate() {
        let configs = [
            stereo(192_000, SampleFormat::F32),
            stereo(40_000, SampleFormat::F32),
        ];

        let selected = select_output_config(&configs, None).unwrap();

        assert_eq!(selected.sample_rate(), 40_000);
    }

    #[test]
    fn output_config_prefers_stereo_over_a_multichannel_default() {
        let configs = [stereo(48_000, SampleFormat::F32)];
        let default =
            SupportedStreamConfig::new(6, 44_100, SupportedBufferSize::Unknown, SampleFormat::F32);

        let selected = select_output_config(&configs, Some(default)).unwrap();

        assert_eq!(selected.channels(), 2);
        assert_eq!(selected.sample_rate(), 48_000);
    }
}
