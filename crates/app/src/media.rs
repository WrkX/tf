//! steuert fremde wiedergabe während einer diktion: pausieren oder die systemlautstärke absenken.

use std::ffi::{c_char, c_int, c_void};
use std::mem::size_of;
use std::ptr::NonNull;
use std::sync::OnceLock;
use std::sync::mpsc;
use std::time::Duration;

use block2::RcBlock;
use dispatch2::{DispatchQoS, DispatchQueue, GlobalQueueIdentifier};
use objc2_core_audio::{
    AudioObjectGetPropertyData, AudioObjectID, AudioObjectIsPropertySettable, AudioObjectPropertyAddress,
    AudioObjectSetPropertyData, kAudioDevicePropertyVolumeScalar, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeOutput,
    kAudioObjectSystemObject,
};
use tf_core::config::PlaybackBehavior;

/// `kMRMediaRemoteSendCommandPlay`.
const PLAY: u32 = 0;
/// `kMRMediaRemoteSendCommandPause`.
const PAUSE: u32 = 1;
/// die abfrage läuft auf einer fremden queue; ohne antwort lieber nichts anfassen.
const REPLY_TIMEOUT: Duration = Duration::from_millis(250);
const MAX_VOLUME_ELEMENT: u32 = 8;
const RTLD_LAZY: c_int = 0x1;

/// hält den zustand, der beim ende der aufnahme zurückgesetzt wird. `Drop` stellt ihn wieder her, damit kein fehlerpfad die lautstärke unten lässt.
pub struct Playback {
    kind: Kind,
}

enum Kind {
    Paused,
    Ducked { device: AudioObjectID, elements: Vec<(u32, f32)> },
}

pub fn begin(behavior: PlaybackBehavior, duck_percent: u32) -> Option<Playback> {
    match behavior {
        PlaybackBehavior::Off => None,
        PlaybackBehavior::Pause => pause(),
        PlaybackBehavior::Duck => duck(duck_percent),
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        match &mut self.kind {
            Kind::Paused => {
                if let Some(remote) = MediaRemote::load()
                    && !remote.send(PLAY)
                {
                    tf_core::log_warn!("could not resume media playback");
                }
            }
            Kind::Ducked { device, elements } => {
                for (element, volume) in elements.drain(..) {
                    if let Err(error) = set_volume(*device, element, volume) {
                        tf_core::log_warn!("could not restore the output volume: {error}");
                    }
                }
            }
        }
    }
}

fn pause() -> Option<Playback> {
    let remote = MediaRemote::load()?;
    match remote.playing()? {
        true if remote.send(PAUSE) => Some(Playback { kind: Kind::Paused }),
        true => {
            tf_core::log_warn!("could not pause media playback");
            None
        }
        false => None,
    }
}

fn duck(percent: u32) -> Option<Playback> {
    let device = match default_output() {
        Some(device) => device,
        None => {
            tf_core::log_warn!("no output device to lower the volume on");
            return None;
        }
    };
    let elements = volume_elements(device);
    if elements.is_empty() {
        tf_core::log_warn!("the output device has no adjustable volume");
        return None;
    }
    let mut saved = Vec::new();
    for element in elements {
        let volume = match get_volume(device, element) {
            Ok(volume) => volume,
            Err(error) => {
                tf_core::log_warn!("could not read the output volume: {error}");
                continue;
            }
        };
        match set_volume(device, element, ducked_volume(volume, percent)) {
            Ok(()) => saved.push((element, volume)),
            Err(error) => tf_core::log_warn!("could not lower the output volume: {error}"),
        }
    }
    (!saved.is_empty()).then_some(Playback { kind: Kind::Ducked { device, elements: saved } })
}

fn ducked_volume(volume: f32, percent: u32) -> f32 {
    let factor = 1.0 - percent.min(100) as f32 / 100.0;
    (volume * factor).clamp(0.0, 1.0)
}

fn volume_address(element: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyVolumeScalar,
        mScope: kAudioObjectPropertyScopeOutput,
        mElement: element,
    }
}

fn default_output() -> Option<AudioObjectID> {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyDefaultOutputDevice,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut device = 0u32;
    let mut length = size_of::<AudioObjectID>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut length),
            NonNull::from(&mut device).cast(),
        )
    };
    (status == 0 && device != 0).then_some(device)
}

/// das hauptelement steuert meist alle kanäle; manche geräte bieten nur die kanäle einzeln an.
fn volume_elements(device: AudioObjectID) -> Vec<u32> {
    if settable(device, kAudioObjectPropertyElementMain) {
        return vec![kAudioObjectPropertyElementMain];
    }
    (1..=MAX_VOLUME_ELEMENT).filter(|element| settable(device, *element)).collect()
}

fn settable(device: AudioObjectID, element: u32) -> bool {
    let address = volume_address(element);
    let mut settable = 0u8;
    let status = unsafe {
        AudioObjectIsPropertySettable(device, NonNull::from(&address), NonNull::from(&mut settable))
    };
    status == 0 && settable != 0
}

fn get_volume(device: AudioObjectID, element: u32) -> Result<f32, String> {
    let address = volume_address(element);
    let mut volume = 0f32;
    let mut length = size_of::<f32>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut length),
            NonNull::from(&mut volume).cast(),
        )
    };
    if status == 0 { Ok(volume) } else { Err(tf_meeting::os_status(status)) }
}

fn set_volume(device: AudioObjectID, element: u32, volume: f32) -> Result<(), String> {
    let address = volume_address(element);
    let mut volume = volume;
    let status = unsafe {
        AudioObjectSetPropertyData(
            device,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            size_of::<f32>() as u32,
            NonNull::from(&mut volume).cast(),
        )
    };
    if status == 0 { Ok(()) } else { Err(tf_meeting::os_status(status)) }
}

type SendCommand = unsafe extern "C" fn(u32, *const c_void) -> u8;
type IsPlaying = unsafe extern "C" fn(*const c_void, *const c_void);

struct MediaRemote {
    send_command: SendCommand,
    is_playing: IsPlaying,
}

impl MediaRemote {
    fn load() -> Option<&'static Self> {
        static REMOTE: OnceLock<Option<MediaRemote>> = OnceLock::new();
        REMOTE.get_or_init(|| unsafe { load_media_remote() }).as_ref()
    }

    fn send(&self, command: u32) -> bool {
        unsafe { (self.send_command)(command, std::ptr::null()) != 0 }
    }

    /// `None`, wenn die antwort ausbleibt; der aufrufer lässt die wiedergabe dann in ruhe.
    fn playing(&self) -> Option<bool> {
        let (sender, receiver) = mpsc::channel();
        let completion = RcBlock::new(move |playing: u8| {
            let _ = sender.send(playing != 0);
        });
        let queue =
            DispatchQueue::global_queue(GlobalQueueIdentifier::QualityOfService(DispatchQoS::Default));
        unsafe {
            (self.is_playing)(
                &*queue as *const DispatchQueue as *const c_void,
                RcBlock::as_ptr(&completion).cast(),
            );
        }
        receiver.recv_timeout(REPLY_TIMEOUT).ok()
    }
}

unsafe fn load_media_remote() -> Option<MediaRemote> {
    let handle = unsafe {
        dlopen(c"/System/Library/PrivateFrameworks/MediaRemote.framework/MediaRemote".as_ptr(), RTLD_LAZY)
    };
    if handle.is_null() {
        tf_core::log_warn!("could not load the system media controls");
        return None;
    }
    let send = unsafe { dlsym(handle, c"MRMediaRemoteSendCommand".as_ptr()) };
    let playing = unsafe { dlsym(handle, c"MRMediaRemoteGetNowPlayingApplicationIsPlaying".as_ptr()) };
    if send.is_null() || playing.is_null() {
        tf_core::log_warn!("media controls are unavailable on this macOS version");
        return None;
    }
    // SAFETY: beide symbole gehören zu den oben erwarteten funktionssignaturen;
    // das framework bleibt für die lebensdauer des prozesses geladen.
    Some(MediaRemote {
        send_command: unsafe { std::mem::transmute::<*mut c_void, SendCommand>(send) },
        is_playing: unsafe { std::mem::transmute::<*mut c_void, IsPlaying>(playing) },
    })
}

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ducking_keeps_a_share_of_the_volume() {
        assert!((ducked_volume(1.0, 80) - 0.2).abs() < f32::EPSILON);
        assert!((ducked_volume(0.5, 40) - 0.3).abs() < f32::EPSILON);
        assert_eq!(ducked_volume(1.0, 100), 0.0);
        assert_eq!(ducked_volume(1.0, 0), 1.0);
        assert_eq!(ducked_volume(1.0, 150), 0.0);
    }
}
