use std::{
    thread,
    time::{Duration, Instant},
};

use async_channel as async_chan;

use librespot::playback::{
    NUM_CHANNELS, SAMPLE_RATE,
    audio_backend::{Sink, SinkError, SinkResult},
    config::AudioFormat,
    convert::Converter,
    decoder::AudioPacket,
};
use sdl2::audio::{AudioFormatNum, AudioQueue, AudioSpecDesired, AudioStatus};

const QUEUE_TARGET: Duration = Duration::from_millis(150);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

/// Something to say before the next song: already decoded, interleaved,
/// and at the playback sample rate, because the audio thread must not be
/// held up decoding.
pub struct NarrationClip {
    pub samples: Vec<f64>,
}

impl NarrationClip {
    fn duration(&self) -> Duration {
        let frames = self.samples.len() / usize::from(NUM_CHANNELS);
        Duration::from_secs_f64(frames as f64 / f64::from(SAMPLE_RATE))
    }
}

/// Opens the output device. SDL refuses to initialize from a second
/// thread and its device handles cross none, so this is the one place
/// audio leaves the process — spoken lines included, which is why the
/// sink takes their inbox.
pub fn low_latency_sdl_sink(
    format: AudioFormat,
    narration: async_chan::Receiver<NarrationClip>,
) -> Box<dyn Sink> {
    Box::new(LowLatencySdlSink {
        device: Device::open(format),
        narration,
        speaking_until: Instant::now(),
    })
}

struct LowLatencySdlSink {
    device: Device,
    narration: async_chan::Receiver<NarrationClip>,
    /// When the queued line finishes. Until then the device queue is
    /// allowed to run that much longer than usual, so song audio lines up
    /// behind the voice instead of the writer stalling on it.
    speaking_until: Instant,
}

enum Device {
    F32(AudioQueue<f32>),
    S32(AudioQueue<i32>),
    S16(AudioQueue<i16>),
    Failed(AudioOpenError),
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

impl Device {
    fn open(format: AudioFormat) -> Self {
        if !matches!(
            format,
            AudioFormat::F32 | AudioFormat::S32 | AudioFormat::S16
        ) {
            return Self::Failed(AudioOpenError::InvalidFormat(format!(
                "SDL does not support {format:?} output"
            )));
        }
        let context = match sdl2::init() {
            Ok(context) => context,
            Err(error) => {
                return Self::Failed(AudioOpenError::Connection(format!(
                    "could not initialize SDL: {error}"
                )));
            }
        };
        let audio = match context.audio() {
            Ok(audio) => audio,
            Err(error) => {
                return Self::Failed(AudioOpenError::Connection(format!(
                    "could not initialize SDL audio subsystem: {error}"
                )));
            }
        };
        let Ok(sample_rate) = i32::try_from(SAMPLE_RATE) else {
            return Self::Failed(AudioOpenError::InvalidFormat(
                "audio sample rate exceeds SDL range".to_owned(),
            ));
        };
        let desired_spec = AudioSpecDesired {
            freq: Some(sample_rate),
            channels: Some(NUM_CHANNELS),
            samples: Some(512),
        };

        match format {
            AudioFormat::F32 => audio.open_queue(None, &desired_spec).map_or_else(
                |error| Self::Failed(AudioOpenError::Connection(error)),
                Self::F32,
            ),
            AudioFormat::S32 => audio.open_queue(None, &desired_spec).map_or_else(
                |error| Self::Failed(AudioOpenError::Connection(error)),
                Self::S32,
            ),
            AudioFormat::S16 => audio.open_queue(None, &desired_spec).map_or_else(
                |error| Self::Failed(AudioOpenError::Connection(error)),
                Self::S16,
            ),
            _ => Self::Failed(AudioOpenError::InvalidFormat(format!(
                "SDL does not support {format:?} output"
            ))),
        }
    }
}

impl Sink for LowLatencySdlSink {
    fn start(&mut self) -> SinkResult<()> {
        self.device.each(
            |queue| {
                queue.clear();
                queue.resume();
            },
            |queue| {
                queue.clear();
                queue.resume();
            },
            |queue| {
                queue.clear();
                queue.resume();
            },
        )
    }

    fn stop(&mut self) -> SinkResult<()> {
        // A skip lands here, and clearing takes the unfinished line with
        // the unfinished song: nobody wants the DJ talking over the next
        // choice.
        self.speaking_until = Instant::now();
        let _ = self.device.each(
            |queue| {
                queue.pause();
                queue.clear();
            },
            |queue| {
                queue.pause();
                queue.clear();
            },
            |queue| {
                queue.pause();
                queue.clear();
            },
        );
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        // Anything the DJ has to say goes in first, so the song queues up
        // behind the voice and follows it without a gap.
        while let Ok(clip) = self.narration.try_recv() {
            let spoken = clip.duration();
            self.enqueue(&clip.samples, converter, Duration::ZERO)?;
            self.speaking_until = self.speaking_until.max(Instant::now()) + spoken;
        }
        let samples = packet
            .samples()
            .map_err(|error| SinkError::OnWrite(error.to_string()))?;
        let spoken_left = self
            .speaking_until
            .saturating_duration_since(Instant::now());
        self.enqueue(samples, converter, spoken_left)
    }
}

impl LowLatencySdlSink {
    /// Queues samples for the device, first waiting for the queue to run
    /// down to the usual few frames — plus however much of a spoken line
    /// is still ahead of them, which is not a backlog to wait out.
    fn enqueue(
        &mut self,
        samples: &[f64],
        converter: &mut Converter,
        allowance: Duration,
    ) -> SinkResult<()> {
        match &self.device {
            Device::F32(queue) => {
                drain_queue(queue, size_of::<f32>(), allowance)?;
                queue
                    .queue_audio(&converter.f64_to_f32(samples))
                    .map_err(SinkError::OnWrite)
            }
            Device::S32(queue) => {
                drain_queue(queue, size_of::<i32>(), allowance)?;
                queue
                    .queue_audio(&converter.f64_to_s32(samples))
                    .map_err(SinkError::OnWrite)
            }
            Device::S16(queue) => {
                drain_queue(queue, size_of::<i16>(), allowance)?;
                queue
                    .queue_audio(&converter.f64_to_s16(samples))
                    .map_err(SinkError::OnWrite)
            }
            Device::Failed(error) => Err(error.sink_error()),
        }
    }
}

impl Device {
    /// Applies whichever of the three sample-type actions fits the open
    /// device, so callers state the action once per type instead of
    /// matching the enum themselves.
    fn each(
        &self,
        f32_action: impl FnOnce(&AudioQueue<f32>),
        s32_action: impl FnOnce(&AudioQueue<i32>),
        s16_action: impl FnOnce(&AudioQueue<i16>),
    ) -> SinkResult<()> {
        match self {
            Self::F32(queue) => f32_action(queue),
            Self::S32(queue) => s32_action(queue),
            Self::S16(queue) => s16_action(queue),
            Self::Failed(error) => return Err(error.sink_error()),
        }
        Ok(())
    }
}

fn drain_queue<T: AudioFormatNum>(
    queue: &AudioQueue<T>,
    sample_size: usize,
    allowance: Duration,
) -> SinkResult<()> {
    let target_bytes = u128::from(SAMPLE_RATE)
        * u128::from(NUM_CHANNELS)
        * sample_size as u128
        * (QUEUE_TARGET + allowance).as_millis()
        / 1000;
    let target_bytes = u32::try_from(target_bytes).unwrap_or(u32::MAX);
    let deadline = std::time::Instant::now() + DRAIN_TIMEOUT;
    while queue.size() > target_bytes {
        if queue.status() != AudioStatus::Playing {
            return Err(SinkError::StateChange(
                "SDL audio device stopped consuming samples".to_owned(),
            ));
        }
        if std::time::Instant::now() >= deadline {
            queue.clear();
            queue.resume();
            return Ok(());
        }
        thread::sleep(Duration::from_millis(2));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_audio_formats_return_sink_errors() {
        let device = Device::open(AudioFormat::F64);

        assert!(matches!(
            device.each(|_| {}, |_| {}, |_| {}),
            Err(SinkError::InvalidParams(_))
        ));
    }
}
