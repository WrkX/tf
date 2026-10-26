
use std::path::Path;

use tf_coreml::{Compute, Model, Tensor};

use crate::Error;
use crate::cluster::Embedding;
use crate::embed::{self, MIN_CLEAN_MS};
use crate::segment::{self, CLASSES, FRAMES, LocalMask, SAMPLE_RATE, WINDOW_SAMPLES, WindowMask};

pub(crate) const MODEL_ID: &str = "speaker-diarization";

pub struct Diarizer {
    pub(crate) segmentation: Model,
    pub(crate) fbank: Model,
    pub(crate) embedding: Model,
}

impl Diarizer {
    pub(crate) fn load_from(dir: &Path) -> Result<Self, Error> {
        let model = |name: &str, compute| Model::load(&dir.join(name), compute);
        if !dir.join("Segmentation.mlmodelc").exists() {
            return Err(Error::MissingModel(dir.to_path_buf()));
        }
        Ok(Self {
            segmentation: model("Segmentation.mlmodelc", Compute::NeuralEngine)?,
            fbank: model("FBank.mlmodelc", Compute::Cpu)?,
            embedding: model("Embedding.mlmodelc", Compute::NeuralEngine)?,
        })
    }

    pub(crate) fn segment_batch(&self, samples: &[f32], starts: &[usize]) -> Result<Vec<WindowMask>, Error> {
        if starts.is_empty() {
            return Ok(Vec::new());
        }
        let audio = Tensor::zeros_f32(&[starts.len(), 1, WINDOW_SAMPLES])?;
        for (row, &start) in starts.iter().enumerate() {
            let end = (start + WINDOW_SAMPLES).min(samples.len());
            if start < end {
                audio.write_f32_at(row * WINDOW_SAMPLES, &samples[start..end]);
            }
        }
        let output = self.segmentation.predict(&[("audio", &audio)])?;
        let log_probs = output.get("log_probs")?;
        let data = log_probs.read_f32();
        let stride = FRAMES * CLASSES;
        let mut masks = Vec::with_capacity(starts.len());
        for row in 0..starts.len() {
            let start = row * stride;
            let end = start + stride;
            if end <= data.len() {
                masks.push(segment::decode(&data[start..end]));
            } else {
                masks.push(WindowMask::default());
            }
        }
        Ok(masks)
    }

    pub(crate) fn embed_window(
        &self,
        samples: &[f32],
        start: usize,
        masks: &[LocalMask],
        window: usize,
    ) -> Result<Vec<Embedding>, Error> {
        let vectors = embed::embed_batch(&self.fbank, &self.embedding, samples, start, masks)?;
        let start_ms = start as u64 * 1_000 / SAMPLE_RATE as u64;
        let mut out = Vec::new();
        for (local, vector) in vectors.into_iter().enumerate() {
            let Some(vector) = vector else {
                continue;
            };
            let duration = masks
                .get(local)
                .map(segment::clean_ms)
                .filter(|duration| *duration >= MIN_CLEAN_MS)
                .unwrap_or(0);
            if duration == 0 {
                continue;
            }
            out.push(Embedding { window, local, start_ms, vector, duration_ms: duration });
        }
        Ok(out)
    }

    /// bettet einen audio-abschnitt eigenständig ein, in chunks von höchstens einem fenster. gibt `None` zurück, wenn der abschnitt unter einer sekunde audio hat.
    pub(crate) fn embed_span(
        &self,
        samples: &[f32],
        start_ms: u64,
        end_ms: u64,
    ) -> Result<Option<Vec<f32>>, Error> {
        let to_sample = |ms: u64| (ms * SAMPLE_RATE as u64 / 1_000) as usize;
        let (begin, end) = (to_sample(start_ms).min(samples.len()), to_sample(end_ms).min(samples.len()));
        let mut chunks: Vec<Vec<f32>> = Vec::new();
        let mut at = begin;
        while end - at.min(end) >= SAMPLE_RATE as usize {
            let length = (end - at).min(WINDOW_SAMPLES);
            let frames =
                ((length as f32 / SAMPLE_RATE as f32 * 1_000.0 / segment::FRAME_MS) as usize).min(FRAMES);
            let mut mask = LocalMask { activity: vec![0.0; FRAMES], overlap: vec![false; FRAMES] };
            mask.activity[..frames].fill(1.0);
            let vectors =
                embed::embed_batch(&self.fbank, &self.embedding, samples, at, std::slice::from_ref(&mask))?;
            chunks.extend(vectors.into_iter().flatten());
            at += length;
        }
        let views = chunks.iter().map(Vec::as_slice).collect::<Vec<_>>();
        Ok((!views.is_empty()).then(|| crate::cluster::mean(&views)))
    }
}
