//! die crate kennt weder ui noch speicherung: app und pipeline-worker besitzen lebensdauer und persistenz. die core-audio-taps sind hier isoliert, damit eine änderung an apples api nur ein modul trifft.

pub mod capture;
pub mod coreaudio;
pub mod detect;
pub mod segmenter;
pub mod summary;

pub use capture::{Capture, CaptureOptions};
pub use detect::{DetectedApp, Detector, MEETING_APPS};
pub use segmenter::{ClosedSegment, Segmenter, Speaker};
pub use summary::{chunks, final_system_prompt, map_system_prompt, transcript};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("core audio: {0}")]
    CoreAudio(String),
    #[error("no microphone is available")]
    NoMicrophone,
    #[error("unsupported audio format: {0}")]
    Format(String),
    #[error("system audio capture permission was denied")]
    PermissionDenied,
    #[error("audio: {0}")]
    Audio(String),
}

pub fn os_status(status: i32) -> String {
    if status == 0 {
        return "ok".to_owned();
    }
    let bytes = (status as u32).to_be_bytes();
    let printable = bytes.iter().all(|byte| byte.is_ascii_graphic() || *byte == b' ');
    if printable {
        format!("{} ({status})", String::from_utf8_lossy(&bytes))
    } else {
        format!("OSStatus {status}")
    }
}
