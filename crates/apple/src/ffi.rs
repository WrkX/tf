use std::ffi::{CStr, c_char};

unsafe extern "C" {
    pub fn tf_string_free(pointer: *mut c_char);
    pub fn tf_fm_availability() -> i32;
    pub fn tf_fm_respond(
        instructions: *const c_char,
        prompt: *const c_char,
        output: *mut *mut c_char,
    ) -> bool;
    pub fn tf_fm_respond_options(
        instructions: *const c_char,
        prompt: *const c_char,
        temperature: f64,
        max_tokens: i32,
        output: *mut *mut c_char,
        code: *mut i32,
    ) -> bool;
    pub fn tf_fm_prewarm();
    pub fn tf_speech_locales(output: *mut *mut c_char) -> bool;
    pub fn tf_speech_install(
        locale: *const c_char,
        progress: Option<extern "C" fn(*mut std::ffi::c_void, f64)>,
        context: *mut std::ffi::c_void,
        output: *mut *mut c_char,
    ) -> bool;
    pub fn tf_speech_transcribe(
        samples: *const f32,
        count: usize,
        locale: *const c_char,
        vocabulary: *const c_char,
        output: *mut *mut c_char,
    ) -> bool;
}

/// übernimmt den besitz eines von swift allokierten strings.
pub fn take_string(pointer: *mut c_char) -> String {
    if pointer.is_null() {
        return String::new();
    }
    let text = unsafe { CStr::from_ptr(pointer) }.to_string_lossy().into_owned();
    unsafe { tf_string_free(pointer) };
    text
}
