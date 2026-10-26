mod tdt;
mod vocab;
mod window;

use std::path::Path;

use tdt::TdtDecoder;
use tf_coreml::{Compute, Model, Tensor};
use vocab::Vocab;
use window::WINDOW_SAMPLES;

use crate::{Error, TranscribeOptions, Transcriber};

/// nvidia parakeet tdt 0.6b auf coreml (fluidinference-konvertierung).
pub struct Parakeet {
    preprocessor: Model,
    encoder: Model,
    decoder: Model,
    joint: Model,
    vocab: Vocab,
    audio: Tensor,
}

impl Parakeet {
    pub fn load(dir: &Path) -> Result<Self, Error> {
        if !dir.join("Preprocessor.mlmodelc").exists() {
            return Err(Error::MissingModel(dir.to_path_buf()));
        }
        let model = |name: &str, compute| Model::load(&dir.join(name), compute);
        Ok(Self {
            preprocessor: model("Preprocessor.mlmodelc", Compute::Cpu)?,
            encoder: model("Encoder.mlmodelc", Compute::NeuralEngine)?,
            decoder: model("Decoder.mlmodelc", Compute::NeuralEngine)?,
            joint: model(joint_file(dir), Compute::NeuralEngine)?,
            vocab: Vocab::load(&dir.join("parakeet_vocab.json"))?,
            audio: Tensor::zeros_f32(&[1, WINDOW_SAMPLES])?,
        })
    }

    fn encode(&self, samples: &[f32]) -> Result<Vec<f32>, Error> {
        let mut padded = samples.to_vec();
        padded.resize(WINDOW_SAMPLES, 0.0);
        self.audio.write_f32(&padded);
        let length = Tensor::i32(&[1], samples.len() as i32)?;

        let mel = self.preprocessor.predict(&[("audio_signal", &self.audio), ("audio_length", &length)])?;
        let (mel, mel_length) = (mel.get("mel")?, mel.get("mel_length")?);
        let encoded = self.encoder.predict(&[("mel", &mel), ("mel_length", &mel_length)])?;

        let valid = encoded.get("encoder_length")?.first_i32().max(0) as usize;
        let mut frames = encoded.get("encoder")?.frame_major();
        frames.truncate(valid * tdt::ENCODER_DIM);
        Ok(frames)
    }
}

impl Transcriber for Parakeet {
    fn transcribe(&mut self, samples: &[f32], _options: &TranscribeOptions) -> Result<String, Error> {
        let blank = self.vocab.len() as u32;
        let mut tdt = TdtDecoder::new(&self.decoder, &self.joint, blank)?;
        for w in window::plan(samples.len()) {
            let frames = self.encode(&samples[w.samples])?;
            tdt.decode(&frames, w.first_frame, w.end_frame)?;
        }
        Ok(self.vocab.decode(&tdt.into_tokens()))
    }
}

fn joint_file(dir: &Path) -> &'static str {
    if dir.join("JointDecisionv3.mlmodelc").exists() {
        "JointDecisionv3.mlmodelc"
    } else {
        "JointDecision.mlmodelc"
    }
}
