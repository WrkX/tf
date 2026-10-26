//! ein privates aggregate-device enthält den tap und, wenn möglich, das mikrofon. ein einziger ioproc-callback liefert dann beide ströme als ausgerichteten block. scheitert das aggregate mit mikrofon (manche usb- und bluetooth-geräte), bekommt der tap sein eigenes aggregate und das mikrofon läuft über cpal; der meeting-thread leert beide im gleichen wall-clock-takt.

use std::collections::VecDeque;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};

use block2::RcBlock;
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_audio::{
    AudioDeviceCreateIOProcIDWithBlock, AudioDeviceDestroyIOProcID, AudioDeviceStart, AudioDeviceStop,
    AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap, AudioHardwareDestroyAggregateDevice,
    AudioHardwareDestroyProcessTap, AudioObjectID, CATapDescription, CATapMuteBehavior,
    kAudioAggregateDeviceClockDeviceKey, kAudioAggregateDeviceIsPrivateKey,
    kAudioAggregateDeviceMainSubDeviceKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceSubDeviceListKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey, kAudioSubDeviceUIDKey,
    kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey, kAudioTapPropertyUID,
};
use objc2_core_audio_types::{AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp};
use objc2_foundation::{NSArray, NSMutableDictionary, NSNumber, NSString, NSUUID};

use crate::coreaudio;
use crate::{Error, os_status};

use super::Speaker;

const BUFFER_SECS: usize = 30;

#[derive(Clone, Debug, Default)]
pub struct CaptureOptions {
    /// coreaudio-uid des mikrofons. `None` nimmt den system-default.
    pub mic_uid: Option<String>,
    /// pids, deren ausgabe getappt wird. leer = jede system-ausgabe außer tf.
    pub app_pids: Vec<i32>,
    /// schließt tf selbst aus einem globalen tap aus.
    pub exclude_own_process: bool,
}

pub struct Capture {
    tap: AudioObjectID,
    aggregate: AudioObjectID,
    io_proc: Option<objc2_core_audio::AudioDeviceIOProcID>,
    mic: Option<tf_audio::Recorder>,
    me: Arc<Buffers>,
    them: Arc<Buffers>,
    rate: u32,
    mic_rate: u32,
    echo_likely: bool,
}

struct Buffers {
    data: Mutex<VecDeque<f32>>,
    capacity: usize,
}

impl Buffers {
    fn new(rate: u32) -> Self {
        Self {
            data: Mutex::new(VecDeque::with_capacity(rate as usize * BUFFER_SECS)),
            capacity: rate as usize * BUFFER_SECS,
        }
    }

    fn push(&self, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }
        let mut data = match self.data.lock() {
            Ok(data) => data,
            Err(poisoned) => poisoned.into_inner(),
        };
        let excess = data.len() + samples.len();
        if excess > self.capacity {
            let drop = excess - self.capacity;
            let len = data.len();
            data.drain(..drop.min(len));
        }
        data.extend(samples.iter().copied());
    }

    fn drain(&self) -> Vec<f32> {
        let mut data = match self.data.lock() {
            Ok(data) => data,
            Err(poisoned) => poisoned.into_inner(),
        };
        data.drain(..).collect()
    }
}

impl Capture {
    pub fn start(options: &CaptureOptions) -> Result<Self, Error> {
        let mic_device = options
            .mic_uid
            .as_deref()
            .and_then(coreaudio::input_device_by_uid)
            .or_else(coreaudio::default_input)
            .ok_or(Error::NoMicrophone)?;
        let mic_uid = coreaudio::device_uid(mic_device).ok_or(Error::NoMicrophone)?;

        let description = tap_description(options)?;
        let mut tap = 0u32;
        let status = unsafe { AudioHardwareCreateProcessTap(Some(&description), &mut tap) };
        if status != 0 {
            return Err(Error::CoreAudio(os_status(status)));
        }

        let tap_uid = coreaudio::property_string(tap, kAudioTapPropertyUID, coreaudio::scope_input())
            .ok()
            .flatten()
            .or_else(|| Some(unsafe { description.UUID().UUIDString().to_string() }))
            .unwrap_or_else(|| "tf-tap".to_owned());
        let tap_format = coreaudio::tap_format(tap).unwrap_or_else(|_| default_format());
        let tap_channels = tap_format.mChannelsPerFrame.max(1) as usize;

        let (aggregate, has_mic) = match create_aggregate(&tap_uid, Some(&mic_uid)) {
            Ok(aggregate) => (aggregate, true),
            Err(error) => {
                // TODO: fallback testen: den tap behalten und das mikrofon separat öffnen. das ist der dokumentierte fallback für geräte, die keinem aggregate beitreten können.
                eprintln!("tf: aggregate with the microphone failed: {error}");
                (create_aggregate(&tap_uid, None)?, false)
            }
        };

        let format = coreaudio::stream_format(aggregate).unwrap_or(tap_format);
        let rate = format.mSampleRate.max(8_000.0) as u32;
        let me = Arc::new(Buffers::new(rate));
        let them = Arc::new(Buffers::new(rate));

        let io_proc = install_io_proc(aggregate, Arc::clone(&me), Arc::clone(&them), tap_channels)?;

        let mic = if has_mic {
            None
        } else {
            match tf_audio::Recorder::start(options.mic_uid.as_deref()) {
                Ok((recorder, _)) => Some(recorder),
                Err(error) => {
                    eprintln!("tf: microphone fallback failed: {error}");
                    None
                }
            }
        };
        let mic_rate = mic.as_ref().map(tf_audio::Recorder::sample_rate).unwrap_or(rate);

        Ok(Self {
            tap,
            aggregate,
            io_proc,
            mic,
            me,
            them,
            rate,
            mic_rate,
            echo_likely: coreaudio::echo_likely(),
        })
    }

    pub fn them_rate(&self) -> u32 {
        self.rate
    }

    pub fn me_rate(&self) -> u32 {
        self.mic_rate
    }

    pub fn echo_likely(&self) -> bool {
        self.echo_likely
    }

    pub fn drain_me(&self) -> Vec<f32> {
        if let Some(mic) = self.mic.as_ref() {
            return mic.drain();
        }
        self.me.drain()
    }

    pub fn drain_them(&self) -> Vec<f32> {
        self.them.drain()
    }

    pub fn drain(&self, speaker: Speaker) -> Vec<f32> {
        match speaker {
            Speaker::Me => self.drain_me(),
            Speaker::Them => self.drain_them(),
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        if let Some(io_proc) = self.io_proc.take() {
            let _ = unsafe { AudioDeviceStop(self.aggregate, io_proc) };
            let _ = unsafe { AudioDeviceDestroyIOProcID(self.aggregate, io_proc) };
        }
        let _ = unsafe { AudioHardwareDestroyAggregateDevice(self.aggregate) };
        let _ = unsafe { AudioHardwareDestroyProcessTap(self.tap) };
        self.mic.take();
    }
}

fn install_io_proc(
    aggregate: AudioObjectID,
    me: Arc<Buffers>,
    them: Arc<Buffers>,
    tap_channels: usize,
) -> Result<Option<objc2_core_audio::AudioDeviceIOProcID>, Error> {
    let block = RcBlock::new(
        move |_now: NonNull<AudioTimeStamp>,
              input: NonNull<AudioBufferList>,
              _input_time: NonNull<AudioTimeStamp>,
              _output: NonNull<AudioBufferList>,
              _output_time: NonNull<AudioTimeStamp>| {
            let list = unsafe { input.as_ref() };
            let count = list.mNumberBuffers as usize;
            if count == 0 {
                return;
            }
            let buffers = unsafe { std::slice::from_raw_parts(list.mBuffers.as_ptr(), count) };
            let split = count.saturating_sub(tap_channels).max(1).min(count);
            if count == 1 && buffers[0].mNumberChannels > 1 {
                split_interleaved(&buffers[0], &me, &them);
                return;
            }
            let mut scratch = [0.0f32; 4096];
            for (index, buffer) in buffers.iter().enumerate() {
                let length = fill_float(buffer, &mut scratch);
                if length == 0 {
                    continue;
                }
                if index < split {
                    me.push(&scratch[..length]);
                } else {
                    them.push(&scratch[..length]);
                }
            }
        },
    );
    let mut io_proc: objc2_core_audio::AudioDeviceIOProcID = None;
    let status = unsafe {
        AudioDeviceCreateIOProcIDWithBlock(
            NonNull::from(&mut io_proc),
            aggregate,
            None,
            RcBlock::as_ptr(&block) as *mut _,
        )
    };
    if status != 0 {
        return Err(Error::CoreAudio(os_status(status)));
    }
    let status = unsafe { AudioDeviceStart(aggregate, io_proc) };
    if status != 0 {
        if let Some(io_proc) = io_proc {
            let _ = unsafe { AudioDeviceDestroyIOProcID(aggregate, Some(io_proc)) };
        }
        return Err(Error::CoreAudio(os_status(status)));
    }
    Ok(Some(io_proc))
}

fn split_interleaved(buffer: &objc2_core_audio_types::AudioBuffer, me: &Arc<Buffers>, them: &Arc<Buffers>) {
    let channels = buffer.mNumberChannels.max(1) as usize;
    if buffer.mData.is_null() || buffer.mDataByteSize == 0 {
        return;
    }
    let count = buffer.mDataByteSize as usize / std::mem::size_of::<f32>();
    let raw = unsafe { std::slice::from_raw_parts(buffer.mData.cast::<f32>(), count) };
    let mut left = [0.0f32; 2048];
    let mut right = [0.0f32; 2048];
    let mut length = 0;
    for frame in raw.chunks_exact(channels).take(left.len()) {
        left[length] = frame[0];
        if channels > 1 {
            right[length] = frame[1];
        }
        length += 1;
    }
    me.push(&left[..length]);
    them.push(&right[..length]);
}

fn fill_float(buffer: &objc2_core_audio_types::AudioBuffer, out: &mut [f32]) -> usize {
    if buffer.mData.is_null() || buffer.mDataByteSize == 0 {
        return 0;
    }
    let channels = buffer.mNumberChannels.max(1) as usize;
    let count = buffer.mDataByteSize as usize / std::mem::size_of::<f32>();
    let raw = unsafe { std::slice::from_raw_parts(buffer.mData.cast::<f32>(), count) };
    if channels == 1 {
        let length = raw.len().min(out.len());
        out[..length].copy_from_slice(&raw[..length]);
        return length;
    }
    let frames = (raw.len() / channels).min(out.len());
    for (index, frame) in raw.chunks_exact(channels).take(frames).enumerate() {
        out[index] = frame.iter().copied().sum::<f32>() / channels as f32;
    }
    frames
}

fn tap_description(options: &CaptureOptions) -> Result<Retained<CATapDescription>, Error> {
    let processes = options
        .app_pids
        .iter()
        .map(|pid| coreaudio::translate_pid(*pid))
        .filter_map(|object| object.map(NSNumber::numberWithUnsignedInt))
        .collect::<Vec<_>>();
    let list = NSArray::from_retained_slice(&processes);
    let description = if !processes.is_empty() {
        unsafe { CATapDescription::initStereoMixdownOfProcesses(CATapDescription::alloc(), &list) }
    } else {
        let mut exclude = Vec::new();
        if options.exclude_own_process
            && let Some(object) = coreaudio::translate_pid(std::process::id() as i32)
        {
            exclude.push(NSNumber::numberWithUnsignedInt(object));
        }
        let list = NSArray::from_retained_slice(&exclude);
        unsafe { CATapDescription::initStereoGlobalTapButExcludeProcesses(CATapDescription::alloc(), &list) }
    };
    unsafe {
        description.setName(&NSString::from_str("tf meeting tap"));
        description.setPrivate(true);
        description.setMuteBehavior(CATapMuteBehavior::Unmuted);
        description.setProcessRestoreEnabled(true);
    }
    Ok(description)
}

fn create_aggregate(tap_uid: &str, mic_uid: Option<&str>) -> Result<AudioObjectID, Error> {
    let uuid = NSUUID::new().UUIDString().to_string();
    let description: Retained<NSMutableDictionary<NSString, AnyObject>> = NSMutableDictionary::new();
    set(&description, kAudioAggregateDeviceUIDKey, string(&format!("dev.tf.meeting.{uuid}")));
    set(&description, kAudioAggregateDeviceNameKey, string("tf meeting"));
    set(&description, kAudioAggregateDeviceIsPrivateKey, number(true));
    set(&description, kAudioAggregateDeviceTapAutoStartKey, number(true));

    let tap_entry: Retained<NSMutableDictionary<NSString, AnyObject>> = NSMutableDictionary::new();
    set(&tap_entry, kAudioSubTapUIDKey, string(tap_uid));
    set(&tap_entry, kAudioSubTapDriftCompensationKey, number(true));
    let taps = NSArray::from_retained_slice(&[any_retained(tap_entry)]);
    set(&description, kAudioAggregateDeviceTapListKey, any_retained(taps));

    if let Some(mic_uid) = mic_uid {
        let mic_entry: Retained<NSMutableDictionary<NSString, AnyObject>> = NSMutableDictionary::new();
        set(&mic_entry, kAudioSubDeviceUIDKey, string(mic_uid));
        let devices = NSArray::from_retained_slice(&[any_retained(mic_entry)]);
        set(&description, kAudioAggregateDeviceSubDeviceListKey, any_retained(devices));
        set(&description, kAudioAggregateDeviceMainSubDeviceKey, string(mic_uid));
    } else if let Some(clock) = coreaudio::default_output().and_then(coreaudio::device_uid) {
        set(&description, kAudioAggregateDeviceClockDeviceKey, string(&clock));
    }

    let dictionary =
        unsafe { &*(Retained::as_ptr(&description) as *const objc2_core_foundation::CFDictionary) };
    let mut aggregate = 0u32;
    let status = unsafe { AudioHardwareCreateAggregateDevice(dictionary, NonNull::from(&mut aggregate)) };
    if status != 0 {
        return Err(Error::CoreAudio(os_status(status)));
    }
    Ok(aggregate)
}

fn string(value: &str) -> Retained<AnyObject> {
    unsafe { Retained::cast_unchecked(NSString::from_str(value)) }
}

fn number(value: bool) -> Retained<AnyObject> {
    unsafe { Retained::cast_unchecked(NSNumber::numberWithBool(value)) }
}

fn any_retained<T: objc2::Message>(value: Retained<T>) -> Retained<AnyObject> {
    unsafe { Retained::cast_unchecked(value) }
}

fn set(
    dictionary: &NSMutableDictionary<NSString, AnyObject>,
    key: &std::ffi::CStr,
    value: Retained<AnyObject>,
) {
    let key = NSString::from_str(&key.to_string_lossy());
    unsafe { dictionary.setObject_forKey(&value, objc2::runtime::ProtocolObject::from_ref(&*key)) };
}

fn default_format() -> AudioStreamBasicDescription {
    AudioStreamBasicDescription {
        mSampleRate: 48_000.0,
        mFormatID: objc2_core_audio_types::kAudioFormatLinearPCM,
        mFormatFlags: objc2_core_audio_types::kAudioFormatFlagsNativeFloatPacked,
        mBytesPerPacket: 4,
        mFramesPerPacket: 1,
        mBytesPerFrame: 4,
        mChannelsPerFrame: 1,
        mBitsPerChannel: 32,
        mReserved: 0,
    }
}

// der tap enthält konstruktionsbedingt einen float-pcm-stream; das hält die signatur für das fallback-format explizit und macht ein mismatch leicht sichtbar.
#[allow(dead_code)]
fn is_float(format: &AudioStreamBasicDescription) -> bool {
    format.mFormatID == objc2_core_audio_types::kAudioFormatLinearPCM
        && format.mFormatFlags & objc2_core_audio_types::kAudioFormatFlagIsFloat != 0
}
