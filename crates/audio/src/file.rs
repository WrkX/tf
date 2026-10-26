//! dateien sind bei tf immer 16 khz mono float-pcm. der reader akzeptiert aber auch andere raten und kanalzahlen, weil ein künftiger importer und das meeting-feature dateien liefern können, die wir nicht geschrieben haben.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_avf_audio::{
    AVAudioCommonFormat, AVAudioFile, AVAudioFormat, AVAudioPCMBuffer, AVEncoderBitRateKey, AVFormatIDKey,
    AVNumberOfChannelsKey, AVSampleRateKey,
};
use objc2_foundation::{NSDictionary, NSMutableDictionary, NSNumber, NSString, NSURL};

use crate::Error;

const SAMPLE_RATE: f64 = 16_000.0;
const CHANNELS: u32 = 1;
const BIT_RATE: u32 = 32_000;
const CHUNK_FRAMES: u32 = 16_000;
/// `kAudioFormatMPEG4AAC` aus coreaudiotypes, als big-endian-fourcc.
const FORMAT_MPEG4_AAC: u32 = 0x6161_6320;

/// schreibt 16 khz mono-samples als 32-kbps-aac. die temporäre datei plus rename sorgen dafür, dass ein crash nie einen abgeschnittenen take hinterlässt.
pub fn write_m4a(path: &Path, samples: &[f32]) -> Result<(), Error> {
    if samples.is_empty() {
        return Err(Error::File("refusing to write an empty take".into()));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| Error::File(error.to_string()))?;
    }
    let tmp = path.with_extension("m4a.tmp");
    let result = write_audio(&tmp, samples);
    match result {
        Ok(()) => {
            if let Err(error) = fs::rename(&tmp, path) {
                let _ = fs::remove_file(&tmp);
                return Err(Error::File(error.to_string()));
            }
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .map_err(|error| Error::File(error.to_string()))
        }
        Err(error) => {
            let _ = fs::remove_file(&tmp);
            Err(error)
        }
    }
}

/// sequenzieller 16 khz mono-aac-writer für meetings. die datei ist nach einem crash nutzbar; nur die letzten paar sekunden des encoders können verloren gehen.
pub struct AacWriter {
    file: Option<Retained<AVAudioFile>>,
    format: Retained<AVAudioFormat>,
    samples: u64,
}

impl AacWriter {
    pub fn create(path: &Path) -> Result<Self, Error> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| Error::File(error.to_string()))?;
        }
        let url = file_url(path);
        let settings = settings();
        let format = unsafe {
            AVAudioFormat::initWithCommonFormat_sampleRate_channels_interleaved(
                AVAudioFormat::alloc(),
                AVAudioCommonFormat::PCMFormatFloat32,
                SAMPLE_RATE,
                CHANNELS,
                false,
            )
        }
        .ok_or_else(|| Error::File("could not create the AAC output format".into()))?;
        let file = unsafe {
            AVAudioFile::initForWriting_settings_commonFormat_interleaved_error(
                AVAudioFile::alloc(),
                &url,
                &settings,
                AVAudioCommonFormat::PCMFormatFloat32,
                false,
            )
        }
        .map_err(|error| Error::File(error.localizedDescription().to_string()))?;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
        Ok(Self { file: Some(file), format, samples: 0 })
    }

    pub fn write(&mut self, samples: &[f32]) -> Result<(), Error> {
        if samples.is_empty() {
            return Ok(());
        }
        for chunk in samples.chunks(CHUNK_FRAMES as usize) {
            let buffer = unsafe {
                AVAudioPCMBuffer::initWithPCMFormat_frameCapacity(
                    AVAudioPCMBuffer::alloc(),
                    &self.format,
                    chunk.len() as u32,
                )
            }
            .ok_or_else(|| Error::File("could not allocate an audio buffer".into()))?;
            unsafe {
                buffer.setFrameLength(chunk.len() as u32);
                let channels = buffer.floatChannelData();
                if channels.is_null() {
                    return Err(Error::File("the audio buffer is not float PCM".into()));
                }
                let channel = (*channels).as_ptr();
                std::ptr::copy_nonoverlapping(chunk.as_ptr(), channel, chunk.len());
                if let Some(file) = self.file.as_ref() {
                    file.writeFromBuffer_error(&buffer)
                        .map_err(|error| Error::File(error.localizedDescription().to_string()))?;
                }
            }
            self.samples += chunk.len() as u64;
        }
        Ok(())
    }

    pub fn samples(&self) -> u64 {
        self.samples
    }

    /// schließt den encoder. ein zweiter aufruf ist harmlos.
    pub fn finish(mut self) {
        if let Some(file) = self.file.take() {
            unsafe { file.close() };
        }
    }
}

impl Drop for AacWriter {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            unsafe { file.close() };
        }
    }
}

fn write_audio(path: &Path, samples: &[f32]) -> Result<(), Error> {
    let url = file_url(path);
    let settings = settings();
    let format = unsafe {
        AVAudioFormat::initWithCommonFormat_sampleRate_channels_interleaved(
            AVAudioFormat::alloc(),
            AVAudioCommonFormat::PCMFormatFloat32,
            SAMPLE_RATE,
            CHANNELS,
            false,
        )
    }
    .ok_or_else(|| Error::File("could not create the AAC output format".into()))?;

    let file = unsafe {
        AVAudioFile::initForWriting_settings_commonFormat_interleaved_error(
            AVAudioFile::alloc(),
            &url,
            &settings,
            AVAudioCommonFormat::PCMFormatFloat32,
            false,
        )
    }
    .map_err(|error| Error::File(error.localizedDescription().to_string()))?;

    for chunk in samples.chunks(CHUNK_FRAMES as usize) {
        let buffer = unsafe {
            AVAudioPCMBuffer::initWithPCMFormat_frameCapacity(
                AVAudioPCMBuffer::alloc(),
                &format,
                chunk.len() as u32,
            )
        }
        .ok_or_else(|| Error::File("could not allocate an audio buffer".into()))?;
        unsafe {
            buffer.setFrameLength(chunk.len() as u32);
            let channels = buffer.floatChannelData();
            if channels.is_null() {
                return Err(Error::File("the audio buffer is not float PCM".into()));
            }
            let channel = (*channels).as_ptr();
            std::ptr::copy_nonoverlapping(chunk.as_ptr(), channel, chunk.len());
            file.writeFromBuffer_error(&buffer)
                .map_err(|error| Error::File(error.localizedDescription().to_string()))?;
        }
    }
    unsafe { file.close() };
    Ok(())
}

fn settings() -> Retained<NSDictionary<NSString, AnyObject>> {
    let settings: Retained<NSMutableDictionary<NSString, AnyObject>> = NSMutableDictionary::new();
    let format = any(NSNumber::numberWithUnsignedInt(FORMAT_MPEG4_AAC));
    let rate = any(NSNumber::numberWithDouble(SAMPLE_RATE));
    let channels = any(NSNumber::numberWithUnsignedInt(CHANNELS));
    let bit_rate = any(NSNumber::numberWithUnsignedInt(BIT_RATE));

    unsafe {
        put(&settings, AVFormatIDKey, format);
        put(&settings, AVSampleRateKey, rate);
        put(&settings, AVNumberOfChannelsKey, channels);
        put(&settings, AVEncoderBitRateKey, bit_rate);
    }
    unsafe { Retained::cast_unchecked(settings) }
}

/// SAFETY: die globals sind framework-konstanten, und `value` ist eine nsnumber.
unsafe fn put(
    settings: &NSMutableDictionary<NSString, AnyObject>,
    key: Option<&'static NSString>,
    value: Retained<AnyObject>,
) {
    if let Some(key) = key {
        unsafe { settings.setObject_forKey(&value, ProtocolObject::from_ref(key)) };
    }
}

fn any(number: Retained<NSNumber>) -> Retained<AnyObject> {
    // SAFETY: jedes objective-c-objekt ist ein anyobject.
    unsafe { Retained::cast_unchecked(number) }
}

pub fn read(path: &Path) -> Result<Vec<f32>, Error> {
    if !path.exists() {
        return Err(Error::File(format!("{} does not exist", path.display())));
    }
    let url = file_url(path);
    let file = unsafe {
        AVAudioFile::initForReading_commonFormat_interleaved_error(
            AVAudioFile::alloc(),
            &url,
            AVAudioCommonFormat::PCMFormatFloat32,
            false,
        )
    }
    .map_err(|error| Error::File(error.localizedDescription().to_string()))?;

    let format = unsafe { file.processingFormat() };
    let rate = unsafe { format.sampleRate() };
    let channels = unsafe { format.channelCount() } as usize;
    let frames = unsafe { file.length() }.max(0) as usize;
    if frames == 0 {
        return Ok(Vec::new());
    }

    let buffer = unsafe {
        AVAudioPCMBuffer::initWithPCMFormat_frameCapacity(AVAudioPCMBuffer::alloc(), &format, frames as u32)
    }
    .ok_or_else(|| Error::File("could not allocate an audio buffer".into()))?;
    unsafe {
        file.readIntoBuffer_error(&buffer)
            .map_err(|error| Error::File(error.localizedDescription().to_string()))?;
    }
    let length = unsafe { buffer.frameLength() } as usize;
    let mut mono = vec![0.0f32; length];
    unsafe {
        let data = buffer.floatChannelData();
        if data.is_null() || channels == 0 {
            return Err(Error::File("the audio file is not float PCM".into()));
        }
        for channel in 0..channels {
            let samples = (*data.add(channel)).as_ptr();
            for (index, mono_sample) in mono.iter_mut().enumerate().take(length) {
                *mono_sample += *samples.add(index) / channels as f32;
            }
        }
    }
    if (rate - SAMPLE_RATE).abs() > 0.5 {
        return Ok(crate::resample::to_16k(&mono, rate as u32));
    }
    Ok(mono)
}

fn file_url(path: &Path) -> Retained<NSURL> {
    NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn temp_path() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tf-audio-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.join("take.m4a")
    }

    fn rms(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt()
    }

    #[test]
    fn round_trips_sixteen_kilohertz_audio() {
        let path = temp_path();
        let samples: Vec<f32> = (0..16_000).map(|index| (index as f32 * 0.05).sin() * 0.4).collect();
        write_m4a(&path, &samples).unwrap();

        let read_back = read(&path).unwrap();
        // aac hat eine encoder-verzögerung, darum ist die frame-zahl nur ungefähr gleich.
        assert!((read_back.len() as i64 - samples.len() as i64).abs() < 4096);
        let before = rms(&samples);
        let after = rms(&read_back);
        assert!((before - after).abs() < before * 0.12, "rms {before} vs {after}");
        let size = fs::metadata(&path).unwrap().len();
        assert!(size > 1_000, "the file looks empty: {size} bytes");
        assert!(!path.with_extension("m4a.tmp").exists());
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn one_minute_is_roughly_240_kb() {
        let path = temp_path();
        let samples: Vec<f32> = (0..16_000 * 60).map(|index| (index as f32 * 0.02).sin() * 0.2).collect();
        write_m4a(&path, &samples).unwrap();
        let size = fs::metadata(&path).unwrap().len();
        assert!((160_000..=360_000).contains(&size), "60 s file is {size} bytes");
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn missing_file_is_an_error() {
        assert!(read(Path::new("/nonexistent/tf/take.m4a")).is_err());
    }
}
