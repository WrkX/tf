use tf_apple::foundation_models;

use crate::{Error, Refiner, Request};

/// zustandslose session an, damit ein take keinen kontext in den nächsten leckt.
pub struct AppleRefiner;

impl AppleRefiner {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AppleRefiner {
    fn default() -> Self {
        Self::new()
    }
}

impl Refiner for AppleRefiner {
    fn refine(&mut self, request: &Request) -> Result<String, Error> {
        if request.deadline <= std::time::Instant::now() {
            return Err(Error::Timeout);
        }
        foundation_models::respond_with(request.system, request.user, request.temperature, request.max_tokens)
            .map_err(map_error)
    }

    fn warm(&mut self) {
        foundation_models::prewarm();
    }
}

fn map_error(error: foundation_models::GenerationError) -> Error {
    use foundation_models::GenerationError as Source;
    match error {
        Source::Unavailable(reason) => Error::Unavailable(reason),
        Source::Refused(reason) => Error::Refused(reason),
        Source::Failed(reason) => Error::Io(reason),
    }
}
