//! die pipeline besteht aus reinen funktionen über slices, damit decoding, clustering und turn-aufbau ohne core ml testbar sind.

mod assign;
mod cluster;
mod embed;
mod models;
mod segment;
mod split;
mod turns;

use std::path::Path;
use std::sync::atomic::AtomicBool;

pub use assign::match_clusters;
pub use cluster::Embedding;
pub use models::Diarizer;
pub use segment::{FRAME_MS, FRAMES, SAMPLE_RATE, WINDOW_SAMPLES};
pub use split::split_two;
pub use turns::Turn;

#[derive(Clone, Debug)]
pub struct Options {
    /// cosine-distanz, ab der zwei cluster nicht mehr verschmolzen werden.
    pub threshold: f32,
    pub max_speakers: usize,
    /// fensterschritt. pyannote nutzt 1 s; 2,5 s vierteln die kosten.
    pub step_ms: u32,
}

impl Default for Options {
    fn default() -> Self {
        Self { threshold: 0.70, max_speakers: 8, step_ms: 2_500 }
    }
}

#[derive(Clone, Debug)]
pub struct Diarization {
    pub turns: Vec<Turn>,
    pub centroids: Vec<Vec<f32>>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("model file not found: {0}")]
    MissingModel(std::path::PathBuf),
    #[error("Core ML: {0}")]
    CoreMl(String),
    #[error("cancelled")]
    Cancelled,
    #[error("audio is too short")]
    TooShort,
}

impl From<tf_coreml::Error> for Error {
    fn from(error: tf_coreml::Error) -> Self {
        match error {
            tf_coreml::Error::MissingModel(path) => Self::MissingModel(path),
            tf_coreml::Error::CoreMl(message) => Self::CoreMl(message),
        }
    }
}

impl Diarizer {
    pub fn load(models_dir: &Path) -> Result<Self, Error> {
        Self::load_from(&models_dir.join(models::MODEL_ID))
    }

    pub fn run(
        &mut self,
        samples: &[f32],
        options: &Options,
        mut progress: impl FnMut(f32),
        cancel: &AtomicBool,
    ) -> Result<Diarization, Error> {
        // updates unter 5 % fortschritt fallen weg, die ui braucht nicht jeden einzelschritt.
        let mut reported = -1.0f32;
        let mut progress = move |fraction: f32| {
            if fraction >= 1.0 || fraction - reported >= 0.05 {
                reported = fraction;
                progress(fraction);
            }
        };
        if samples.len() < SAMPLE_RATE as usize / 10 {
            return Err(Error::TooShort);
        }
        let windows = segment::plan(samples.len(), window_step(options.step_ms));

        let mut masks = Vec::with_capacity(windows.len());
        for (index, batch) in windows.chunks(embed::BATCH).enumerate() {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            masks.extend(self.segment_batch(samples, batch)?);
            let done = (index + 1) * embed::BATCH;
            progress(0.45 * (done.min(windows.len()) as f32 / windows.len() as f32));
        }

        let mut embeddings = Vec::new();
        for (index, mask) in masks.iter().enumerate() {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            if segment::has_speech(mask) {
                embeddings.extend(self.embed_window(samples, windows[index], &mask.masks, index)?);
            }
            progress(0.45 + 0.35 * ((index + 1) as f32 / masks.len() as f32));
        }

        // nur beobachtungen mit mindestens einer sekunde sauberer sprache werden cluster-mitglied; kürzere landen später beim nächsten centroid. sind alle turns kurz, fallback auf die 300-ms-beobachtungen, damit die besprechung trotzdem sprecher-labels bekommt.
        let mut eligible =
            embeddings.iter().filter(|embedding| embedding.duration_ms >= 1_000).cloned().collect::<Vec<_>>();
        if eligible.is_empty() {
            eligible = embeddings.clone();
        }

        // clustering mit cannot-link für dasselbe fenster. fenster überlappen, eine sekunde sprache wird also `window / step`-mal gesehen; die dauer zurückskalieren, damit die klein-cluster-regel echte zeit sieht.
        let scale = (options.step_ms as f32 / 10_000.0).min(1.0);
        let weighted = eligible
            .iter()
            .map(|embedding| Embedding {
                duration_ms: (embedding.duration_ms as f32 * scale).round() as u64,
                ..embedding.clone()
            })
            .collect::<Vec<_>>();
        let assignment = cluster::cluster(&weighted, options.threshold, options.max_speakers);
        let mut speaker_of = std::collections::HashMap::new();
        for (embedding, speaker) in eligible.iter().zip(&assignment) {
            speaker_of.insert((embedding.window, embedding.local), *speaker);
        }
        // kurze saubere segmente hatten kein eigenes cluster-mitglied; sie an den nächsten centroid hängen, damit ihre frames noch im richtigen turn landen.
        let centroids = cluster::centroids(&eligible, &assignment);
        for embedding in &embeddings {
            if speaker_of.contains_key(&(embedding.window, embedding.local)) {
                continue;
            }
            if let Some(speaker) = cluster::nearest(&embedding.vector, &centroids) {
                speaker_of.insert((embedding.window, embedding.local), speaker);
            }
        }
        for (index, mask) in masks.iter().enumerate() {
            for local in 0..mask.masks.len() {
                if speaker_of.contains_key(&(index, local)) {
                    continue;
                }
                let clean = segment::clean_ms(&mask.masks[local]);
                if clean < 300 {
                    continue;
                }
                if let Some(embedding) = embed::embed_one(
                    &self.fbank,
                    &self.embedding,
                    samples,
                    windows[index],
                    &mask.masks[local],
                )? {
                    let nearest = cluster::nearest(&embedding, &centroids);
                    if let Some(nearest) = nearest {
                        speaker_of.insert((index, local), nearest);
                    }
                }
            }
        }

        progress(0.80);

        let total_ms = samples.len() as u64 * 1_000 / SAMPLE_RATE as u64;
        let (labels, overlaps) = turns::labels(&windows, &masks, &speaker_of, samples.len(), total_ms);
        let mut turns = turns::reconstruct(&labels, &overlaps, total_ms);
        turns::split_long(&mut turns, samples);
        // window-embeddings sind der fallback; ein ausreichend langer turn bekommt ein eigenes.
        turns::attach_embeddings(&mut turns, &eligible, &assignment, &centroids);
        for turn in &mut turns {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            if turn.end_ms - turn.start_ms < 1_000 {
                continue;
            }
            if let Some(own) = self.embed_span(samples, turn.start_ms, turn.end_ms)? {
                turn.embedding = Some(own);
            }
        }

        progress(1.0);

        Ok(Diarization { turns, centroids })
    }
}

fn window_step(step_ms: u32) -> usize {
    ((u64::from(step_ms) * SAMPLE_RATE as u64) / 1_000) as usize
}

/// re-clustert gespeicherte turn-embeddings mit nutzer-korrekturen als randbedingung. `locked[i]` ist der aktuelle sprecher eines gesperrten turns (oder eines turns, dessen zuordnung sich nicht ändern darf). nicht zugeordnete cluster bekommen einen neuen index.
pub fn recluster(
    embeddings: &[Option<Vec<f32>>],
    locked: &[Option<usize>],
    previous: &[Vec<f32>],
    options: &Options,
) -> Vec<Option<usize>> {
    let mut out = vec![None; embeddings.len()];

    let mut locked_ids = locked.iter().flatten().copied().collect::<Vec<_>>();
    locked_ids.sort_unstable();
    locked_ids.dedup();

    for (index, lock) in locked.iter().enumerate() {
        if let Some(lock) = lock {
            out[index] = Some(*lock);
        }
    }

    let locked_centroids = locked_ids
        .iter()
        .map(|id| {
            let members = embeddings
                .iter()
                .zip(locked)
                .filter(|(_, lock)| **lock == Some(*id))
                .filter_map(|(embedding, _)| embedding.as_deref())
                .collect::<Vec<_>>();
            cluster::mean(&members)
        })
        .collect::<Vec<_>>();

    let mut free = Vec::new();
    for (index, (embedding, lock)) in embeddings.iter().zip(locked).enumerate() {
        if lock.is_some() {
            continue;
        }
        let Some(embedding) = embedding else {
            continue;
        };
        let nearest = locked_centroids
            .iter()
            .enumerate()
            .map(|(slot, centroid)| (slot, 1.0 - cluster::cosine(centroid, embedding)))
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((slot, distance)) = nearest
            && distance <= options.threshold
        {
            out[index] = Some(locked_ids[slot]);
            continue;
        }
        free.push(index);
    }

    if !free.is_empty() {
        let items = free
            .iter()
            .map(|index| Embedding {
                window: 0,
                local: 0,
                start_ms: 0,
                vector: embeddings[*index].clone().unwrap_or_default(),
                duration_ms: 1_000,
            })
            .collect::<Vec<_>>();
        let assignment = cluster::cluster(&items, options.threshold, options.max_speakers);
        let centroids = cluster::centroids(&items, &assignment);
        let mapping = match_clusters(&centroids, previous, options.threshold);
        // neue sprecher dürfen keinen index von `previous` belegen.
        let next = locked_ids.iter().max().map(|id| id + 1).unwrap_or(0).max(previous.len());
        let mut new_counter = next;
        let mut ids = Vec::with_capacity(mapping.len());
        for matched in &mapping {
            match matched {
                Some(id) => ids.push(*id),
                None => {
                    ids.push(new_counter);
                    new_counter += 1;
                }
            }
        }
        for (slot, index) in free.iter().enumerate() {
            out[*index] = Some(ids[assignment[slot]]);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_default_to_the_pyannote_threshold() {
        let options = Options::default();
        assert!((options.threshold - 0.70).abs() < f32::EPSILON);
        assert_eq!(options.max_speakers, 8);
        assert_eq!(options.step_ms, 2_500);
    }

    #[test]
    fn recluster_keeps_locked_items_and_matches_previous_speakers() {
        let embeddings = vec![Some(vec![1.0, 0.0]), Some(vec![0.9, 0.1]), Some(vec![0.0, 1.0])];
        let locked = vec![Some(4), None, None];
        let previous = vec![vec![1.0, 0.0], vec![0.0, 1.0]];
        let out = recluster(&embeddings, &locked, &previous, &Options::default());
        assert_eq!(out[0], Some(4));
        assert_eq!(out[1], Some(4));
        assert_eq!(out[2], Some(1));
    }

    #[test]
    fn recluster_gives_unmatched_clusters_fresh_indices() {
        let embeddings = vec![Some(vec![1.0, 0.0]), Some(vec![0.0, 1.0])];
        let previous = vec![vec![1.0, 0.0]];
        let out = recluster(&embeddings, &[None, None], &previous, &Options::default());
        assert_eq!(out[0], Some(0));
        assert_eq!(out[1], Some(1));
    }

    #[test]
    fn recluster_keeps_items_without_an_embedding_unassigned() {
        let out = recluster(&[None], &[None], &[], &Options::default());
        assert_eq!(out, vec![None]);
    }
}
