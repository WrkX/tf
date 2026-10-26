use std::ffi::CString;
use std::ptr;

use crate::{Error, ffi};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Available,
    DeviceNotEligible,
    AppleIntelligenceDisabled,
    ModelNotReady,
    Unavailable,
}

impl Availability {
    pub fn reason(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::DeviceNotEligible => "this Mac is not eligible for Apple Intelligence",
            Self::AppleIntelligenceDisabled => "Apple Intelligence is turned off",
            Self::ModelNotReady => "the on-device model is still downloading",
            Self::Unavailable => "Foundation Models is unavailable",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GenerationError {
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Refused(String),
    #[error("{0}")]
    Failed(String),
}

pub fn availability() -> Availability {
    match unsafe { ffi::tf_fm_availability() } {
        0 => Availability::Available,
        1 => Availability::DeviceNotEligible,
        2 => Availability::AppleIntelligenceDisabled,
        3 => Availability::ModelNotReady,
        _ => Availability::Unavailable,
    }
}

pub fn respond(instructions: &str, prompt: &str) -> Result<String, Error> {
    let instructions = c_string(instructions)?;
    let prompt = c_string(prompt)?;
    let mut output = ptr::null_mut();
    let ok = unsafe { ffi::tf_fm_respond(instructions.as_ptr(), prompt.as_ptr(), &mut output) };
    let text = ffi::take_string(output);
    if ok { Ok(text) } else { Err(Error(text)) }
}

pub fn respond_with(
    instructions: &str,
    prompt: &str,
    temperature: f32,
    max_tokens: u32,
) -> Result<String, GenerationError> {
    let instructions =
        CString::new(instructions).map_err(|_| GenerationError::Failed("text contains a NUL byte".into()))?;
    let prompt =
        CString::new(prompt).map_err(|_| GenerationError::Failed("text contains a NUL byte".into()))?;
    let mut output = ptr::null_mut();
    let mut code = 0i32;
    let ok = unsafe {
        ffi::tf_fm_respond_options(
            instructions.as_ptr(),
            prompt.as_ptr(),
            f64::from(temperature.clamp(0.0, 2.0)),
            max_tokens.min(i32::MAX as u32) as i32,
            &mut output,
            &mut code,
        )
    };
    let text = ffi::take_string(output);
    if ok {
        return Ok(text);
    }
    Err(match code {
        1 => GenerationError::Unavailable(availability().reason().to_owned()),
        2 => GenerationError::Refused(text),
        _ => GenerationError::Failed(text),
    })
}

/// lädt das on-device-modell vor der ersten anfrage.
pub fn prewarm() {
    unsafe { ffi::tf_fm_prewarm() };
}

fn c_string(s: &str) -> Result<CString, Error> {
    CString::new(s).map_err(|_| Error("text contains a NUL byte".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn availability_has_a_reason() {
        assert_eq!(Availability::Available.reason(), "available");
        assert!(Availability::AppleIntelligenceDisabled.reason().contains("Apple Intelligence"));
    }
}
