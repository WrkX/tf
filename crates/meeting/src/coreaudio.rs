//! dünne, typisierte wrapper um die core-audio-hal-property-api.

use std::ffi::c_void;
use std::mem::{MaybeUninit, size_of};
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2_core_audio::{
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID, AudioObjectPropertyAddress,
    kAudioDevicePropertyDeviceUID, kAudioDevicePropertyTransportType,
    kAudioHardwarePropertyDefaultInputDevice, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioHardwarePropertyProcessObjectList, kAudioHardwarePropertyTranslatePIDToProcessObject,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeInput,
    kAudioObjectSystemObject, kAudioProcessPropertyBundleID, kAudioProcessPropertyIsRunningInput,
    kAudioProcessPropertyPID, kAudioTapPropertyFormat,
};
use objc2_foundation::NSString;

use crate::{Error, os_status};

fn address(selector: u32, scope: u32, element: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress { mSelector: selector, mScope: scope, mElement: element }
}

fn size(object: AudioObjectID, selector: u32, scope: u32) -> Result<u32, Error> {
    let address = address(selector, scope, kAudioObjectPropertyElementMain);
    let mut size = 0u32;
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            object,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
        )
    };
    if status != 0 {
        return Err(Error::CoreAudio(os_status(status)));
    }
    Ok(size)
}

pub fn property<T: Copy>(object: AudioObjectID, selector: u32, scope: u32) -> Result<T, Error> {
    let address = address(selector, scope, kAudioObjectPropertyElementMain);
    let mut value = MaybeUninit::<T>::uninit();
    let mut length = size_of::<T>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut length),
            NonNull::new_unchecked(value.as_mut_ptr().cast::<c_void>()),
        )
    };
    if status != 0 {
        return Err(Error::CoreAudio(os_status(status)));
    }
    if length < size_of::<T>() as u32 {
        return Err(Error::CoreAudio("property was shorter than expected".into()));
    }
    Ok(unsafe { value.assume_init() })
}

pub fn property_vec<T: Copy>(object: AudioObjectID, selector: u32, scope: u32) -> Result<Vec<T>, Error> {
    let length = size(object, selector, scope)?;
    let count = length as usize / size_of::<T>().max(1);
    if count == 0 {
        return Ok(Vec::new());
    }
    let address = address(selector, scope, kAudioObjectPropertyElementMain);
    let mut values: Vec<T> = Vec::with_capacity(count);
    let mut length = length;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut length),
            NonNull::new_unchecked(values.as_mut_ptr().cast::<c_void>()),
        )
    };
    if status != 0 {
        return Err(Error::CoreAudio(os_status(status)));
    }
    unsafe { values.set_len(count) };
    Ok(values)
}

pub fn property_string(object: AudioObjectID, selector: u32, scope: u32) -> Result<Option<String>, Error> {
    let length = size(object, selector, scope)?;
    if length < size_of::<*const c_void>() as u32 {
        return Ok(None);
    }
    let address = address(selector, scope, kAudioObjectPropertyElementMain);
    let mut raw: *const c_void = std::ptr::null();
    let mut length = size_of::<*const c_void>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut length),
            NonNull::new_unchecked((&mut raw as *mut *const c_void).cast::<c_void>()),
        )
    };
    if status != 0 || raw.is_null() {
        return Ok(None);
    }
    // die hal liefert einen +1-cfstring, der toll-free an nsstring gebrückt ist.
    let value = unsafe { Retained::from_raw(raw as *mut NSString) };
    Ok(value.map(|value| value.to_string()))
}

pub fn translate_pid(pid: i32) -> Option<AudioObjectID> {
    let address = address(
        kAudioHardwarePropertyTranslatePIDToProcessObject,
        kAudioObjectPropertyScopeGlobal,
        kAudioObjectPropertyElementMain,
    );
    let mut object = 0u32;
    let mut length = size_of::<u32>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&address),
            size_of::<i32>() as u32,
            (&pid as *const i32).cast(),
            NonNull::from(&mut length),
            NonNull::new_unchecked((&mut object as *mut u32).cast::<c_void>()),
        )
    };
    (status == 0 && object != 0).then_some(object)
}

pub fn process_list() -> Vec<AudioObjectID> {
    property_vec(
        kAudioObjectSystemObject as AudioObjectID,
        kAudioHardwarePropertyProcessObjectList,
        kAudioObjectPropertyScopeGlobal,
    )
    .unwrap_or_default()
}

pub fn process_pid(object: AudioObjectID) -> Option<i32> {
    property::<i32>(object, kAudioProcessPropertyPID, kAudioObjectPropertyScopeGlobal).ok()
}

pub fn process_bundle(object: AudioObjectID) -> Option<String> {
    property_string(object, kAudioProcessPropertyBundleID, kAudioObjectPropertyScopeGlobal).ok().flatten()
}

pub fn process_running_input(object: AudioObjectID) -> bool {
    property::<u32>(object, kAudioProcessPropertyIsRunningInput, kAudioObjectPropertyScopeGlobal)
        .map(|value| value != 0)
        .unwrap_or(false)
}

pub fn default_input() -> Option<AudioObjectID> {
    property::<u32>(
        kAudioObjectSystemObject as AudioObjectID,
        kAudioHardwarePropertyDefaultInputDevice,
        kAudioObjectPropertyScopeGlobal,
    )
    .ok()
    .filter(|id| *id != 0)
}

pub fn default_output() -> Option<AudioObjectID> {
    property::<u32>(
        kAudioObjectSystemObject as AudioObjectID,
        kAudioHardwarePropertyDefaultOutputDevice,
        kAudioObjectPropertyScopeGlobal,
    )
    .ok()
    .filter(|id| *id != 0)
}

pub fn device_uid(object: AudioObjectID) -> Option<String> {
    property_string(object, kAudioDevicePropertyDeviceUID, kAudioObjectPropertyScopeGlobal).ok().flatten()
}

pub fn input_device_by_uid(uid: &str) -> Option<AudioObjectID> {
    let input = default_input();
    if let Some(input) = input
        && device_uid(input).as_deref() == Some(uid)
    {
        return Some(input);
    }
    // ein gepinntes gerät ist meist nicht das default, also alle geräte durchgehen. cpal bietet dieselbe liste, aber die hal-id hält das aggregate-dictionary frei von zusätzlichen übersetzungen.
    property_vec::<u32>(
        kAudioObjectSystemObject as AudioObjectID,
        objc2_core_audio::kAudioHardwarePropertyDevices,
        kAudioObjectPropertyScopeGlobal,
    )
    .unwrap_or_default()
    .into_iter()
    .find(|device| device_uid(*device).as_deref() == Some(uid))
}

/// ob die default-ausgabe wahrscheinlich ins mikrofon einstreut (lautsprecher, hdmi, airplay) statt eines headsets.
pub fn echo_likely() -> bool {
    use objc2_core_audio::{
        kAudioDeviceTransportTypeAirPlay, kAudioDeviceTransportTypeBuiltIn,
        kAudioDeviceTransportTypeDisplayPort, kAudioDeviceTransportTypeHDMI,
    };
    let Some(output) = default_output() else {
        return false;
    };
    let transport =
        property::<u32>(output, kAudioDevicePropertyTransportType, kAudioObjectPropertyScopeGlobal)
            .unwrap_or(0);
    transport == kAudioDeviceTransportTypeBuiltIn
        || transport == kAudioDeviceTransportTypeHDMI
        || transport == kAudioDeviceTransportTypeDisplayPort
        || transport == kAudioDeviceTransportTypeAirPlay
}

pub fn stream_format(
    object: AudioObjectID,
) -> Result<objc2_core_audio_types::AudioStreamBasicDescription, Error> {
    property(object, objc2_core_audio::kAudioDevicePropertyStreamFormat, kAudioObjectPropertyScopeInput)
}

/// taps liefern ihr format über `kAudioTapPropertyFormat`, nicht über die stream-property des geräts.
pub fn tap_format(
    object: AudioObjectID,
) -> Result<objc2_core_audio_types::AudioStreamBasicDescription, Error> {
    property(object, kAudioTapPropertyFormat, kAudioObjectPropertyScopeGlobal)
}

pub fn scope_input() -> u32 {
    kAudioObjectPropertyScopeInput
}
