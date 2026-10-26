use crate::{Error, TranscribeOptions, Transcriber};

pub struct AppleSpeech {
    locale: String,
}

impl AppleSpeech {
    pub fn new(locale: String) -> Self {
        Self { locale }
    }
}

impl Transcriber for AppleSpeech {
    fn transcribe(&mut self, samples: &[f32], options: &TranscribeOptions) -> Result<String, Error> {
        tf_apple::speech::transcribe(samples, &self.locale, options.vocabulary)
            .map_err(|e| Error::AppleSpeech(e.to_string()))
    }
}
