pub mod file;
pub mod resample;
pub mod vad;

mod level;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, Stream};

pub use level::{Meter, normalize};

/// 60 s headroom bei der geräte-rate, damit der callback im steady state selten allokiert.
const RESERVE_SECS: u32 = 60;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no input device available")]
    NoDevice,
    #[error("unsupported sample format {0:?}")]
    UnsupportedFormat(SampleFormat),
    #[error("audio file: {0}")]
    File(String),
    #[error(transparent)]
    Device(#[from] cpal::Error),
}

pub struct Recording {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

impl Recording {
    pub fn duration(&self) -> std::time::Duration {
        if self.sample_rate == 0 {
            std::time::Duration::ZERO
        } else {
            std::time::Duration::from_secs_f64(self.samples.len() as f64 / f64::from(self.sample_rate))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputDevice {
    /// coreaudio-geräte-uid, stabil über neustarts und erneutes verbinden.
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

pub fn devices() -> Vec<InputDevice> {
    let host = cpal::default_host();
    let default = host.default_input_device().and_then(|device| device.id().ok());
    let Ok(devices) = host.input_devices() else {
        return Vec::new();
    };
    devices
        .filter_map(|device| {
            let id = device.id().ok()?;
            let name = device
                .description()
                .map(|description| description.name().to_owned())
                .unwrap_or_else(|_| id.id().to_owned());
            Some(InputDevice { id: id.id().to_owned(), name, is_default: default.as_ref() == Some(&id) })
        })
        .collect()
}

fn input_by_uid(uid: &str) -> Option<cpal::Device> {
    let host = cpal::default_host();
    host.input_devices().ok()?.find(|device| device.id().ok().map(|id| id.id() == uid).unwrap_or(false))
}

pub struct Recorder {
    stream: Stream,
    shared: Arc<Shared>,
    sample_rate: u32,
}

struct Shared {
    samples: Mutex<Vec<f32>>,
    level: AtomicU32,
}

impl Shared {
    fn new(capacity: usize) -> Self {
        Self { samples: Mutex::new(Vec::with_capacity(capacity)), level: AtomicU32::new(0) }
    }

    fn push<T: Copy>(&self, interleaved: &[T], channels: usize, convert: impl Fn(T) -> f32) {
        let channels = channels.max(1);
        let mut samples = match self.samples.lock() {
            Ok(samples) => samples,
            Err(poisoned) => poisoned.into_inner(),
        };
        let start = samples.len();
        if channels == 1 {
            samples.extend(interleaved.iter().map(|&s| convert(s)));
        } else {
            samples.reserve(interleaved.len() / channels + 1);
            for frame in interleaved.chunks_exact(channels) {
                let sum: f32 = frame.iter().map(|&s| convert(s)).sum();
                samples.push(sum / channels as f32);
            }
        }
        let rms = level::rms(&samples[start..]);
        self.level.store(rms.to_bits(), Ordering::Relaxed);
    }
}

impl Recorder {
    /// öffnet das gepinnte gerät per uid, sonst den system-default. der zweite rückgabewert ist die fehlende uid, wenn das gepinnte gerät nicht verbunden war und stattdessen der default genutzt wurde.
    pub fn start(device: Option<&str>) -> Result<(Self, Option<String>), Error> {
        let pinned = device.and_then(input_by_uid);
        let (device, fallback) = match (device, pinned) {
            (Some(uid), None) => {
                (cpal::default_host().default_input_device().ok_or(Error::NoDevice)?, Some(uid.to_owned()))
            }
            (Some(_), Some(device)) => (device, None),
            (None, _) => (cpal::default_host().default_input_device().ok_or(Error::NoDevice)?, None),
        };
        let supported = device.default_input_config()?;
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let channels = usize::from(config.channels);
        let sample_rate = config.sample_rate;
        let shared = Arc::new(Shared::new(sample_rate as usize * RESERVE_SECS as usize));

        let stream = match format {
            SampleFormat::F32 => open::<f32>(&device, config, Arc::clone(&shared), channels),
            SampleFormat::F64 => open::<f64>(&device, config, Arc::clone(&shared), channels),
            SampleFormat::I16 => open::<i16>(&device, config, Arc::clone(&shared), channels),
            SampleFormat::U16 => open::<u16>(&device, config, Arc::clone(&shared), channels),
            SampleFormat::I32 => open::<i32>(&device, config, Arc::clone(&shared), channels),
            SampleFormat::U32 => open::<u32>(&device, config, Arc::clone(&shared), channels),
            SampleFormat::I8 => open::<i8>(&device, config, Arc::clone(&shared), channels),
            SampleFormat::U8 => open::<u8>(&device, config, Arc::clone(&shared), channels),
            SampleFormat::I64 => open::<i64>(&device, config, Arc::clone(&shared), channels),
            SampleFormat::U64 => open::<u64>(&device, config, Arc::clone(&shared), channels),
            other => Err(Error::UnsupportedFormat(other)),
        }?;
        stream.play()?;
        Ok((Self { stream, shared, sample_rate }, fallback))
    }

    /// letzter rms-pegel des inputs, `0.0..=1.0`.
    pub fn level(&self) -> f32 {
        f32::from_bits(self.shared.level.load(Ordering::Relaxed))
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// holt die aufgenommenen samples raus, ohne den stream zu stoppen. die meeting-pipeline drainiert in kurzen blöcken, während die aufnahme läuft.
    pub fn drain(&self) -> Vec<f32> {
        let mut samples = match self.shared.samples.lock() {
            Ok(samples) => samples,
            Err(poisoned) => poisoned.into_inner(),
        };
        std::mem::take(&mut *samples)
    }

    pub fn stop(self) -> Recording {
        drop(self.stream);
        let samples = match self.shared.samples.lock() {
            Ok(mut samples) => std::mem::take(&mut *samples),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        };
        Recording { samples, sample_rate: self.sample_rate }
    }
}

fn open<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    shared: Arc<Shared>,
    channels: usize,
) -> Result<Stream, Error>
where
    T: SizedSample + Sample,
    f32: FromSample<T>,
{
    let writer = Arc::clone(&shared);
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _| writer.push(data, channels, |s| f32::from_sample(s)),
        |e| eprintln!("tf: audio stream error: {e}"),
        None,
    )?;
    Ok(stream)
}
