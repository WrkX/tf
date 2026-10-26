use std::ffi::{CString, c_void};
use std::ptr;

use crate::{Error, ffi};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeechLocale {
    pub id: String,
    pub name: String,
    pub installed: bool,
}

pub fn locales() -> Result<Vec<SpeechLocale>, Error> {
    let mut output = ptr::null_mut();
    let ok = unsafe { ffi::tf_speech_locales(&mut output) };
    let text = ffi::take_string(output);
    if !ok {
        return Err(Error(text));
    }
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| Error(e.to_string()))?;
    let mut locales = Vec::new();
    for item in value.as_array().into_iter().flatten() {
        let id = item.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_owned();
        if id.is_empty() {
            continue;
        }
        locales.push(SpeechLocale {
            name: item.get("name").and_then(|v| v.as_str()).unwrap_or(&id).to_owned(),
            installed: item.get("installed").and_then(|v| v.as_bool()).unwrap_or(false),
            id,
        });
    }
    locales.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(locales)
}

pub fn install(locale: &str, progress: impl Fn(f64) + Send + 'static) -> Result<(), Error> {
    let locale = CString::new(locale).map_err(|_| Error("locale contains a NUL byte".into()))?;
    let boxed: Box<Box<dyn Fn(f64) + Send>> = Box::new(Box::new(progress));
    let context = Box::into_raw(boxed) as *mut c_void;
    let mut output = ptr::null_mut();
    let ok = unsafe { ffi::tf_speech_install(locale.as_ptr(), Some(trampoline), context, &mut output) };
    // der swift-monitor-task kann beim zurückkehren des synchronen aufrufs noch am unwinden sein, also die kleine box leaken statt use-after-free zu riskieren.
    let text = ffi::take_string(output);
    if ok { Ok(()) } else { Err(Error(text)) }
}

extern "C" fn trampoline(context: *mut c_void, value: f64) {
    if context.is_null() {
        return;
    }
    // SAFETY: der pointer kommt aus `install`, das die box für den ganzen synchronen aufruf am leben hält.
    let callback = unsafe { &*(context as *const Box<dyn Fn(f64) + Send>) };
    callback(value);
}

pub fn transcribe(samples: &[f32], locale: &str, vocabulary: &[String]) -> Result<String, Error> {
    let locale = CString::new(locale).map_err(|_| Error("locale contains a NUL byte".into()))?;
    let vocabulary = serde_json::to_string(vocabulary).map_err(|e| Error(e.to_string()))?;
    let vocabulary = CString::new(vocabulary).map_err(|_| Error("vocabulary contains a NUL byte".into()))?;
    let mut output = ptr::null_mut();
    let ok = unsafe {
        ffi::tf_speech_transcribe(
            samples.as_ptr(),
            samples.len(),
            locale.as_ptr(),
            vocabulary.as_ptr(),
            &mut output,
        )
    };
    let text = ffi::take_string(output);
    if ok { Ok(text) } else { Err(Error(text)) }
}
