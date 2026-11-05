//! spracherkennung über das NaturalLanguage-framework. schnell genug für den aufruf pro take.

use std::ffi::CString;
use std::ptr;

use crate::ffi;

/// die dominante sprache als bcp-47-code wie `de`. mit `candidates` fällt die wahl nur zwischen diesen.
pub fn detect(text: &str, candidates: &[&str]) -> Option<String> {
    let text = CString::new(text).ok()?;
    let candidates = CString::new(candidates.join(",")).ok()?;
    let mut output = ptr::null_mut();
    let ok = unsafe { ffi::tf_language_detect(text.as_ptr(), candidates.as_ptr(), &mut output) };
    ok.then(|| ffi::take_string(output))
}

/// englischer name eines sprachcodes, z. b. `German` für `de`.
pub fn name(code: &str) -> Option<String> {
    let code = CString::new(code).ok()?;
    let mut output = ptr::null_mut();
    let ok = unsafe { ffi::tf_language_name(code.as_ptr(), &mut output) };
    ok.then(|| ffi::take_string(output))
}
