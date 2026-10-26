
use tf_coreml::{Model, Tensor};

use crate::Error;
use crate::cluster;
use crate::segment::{FRAMES, LocalMask, WINDOW_SAMPLES, clean_ms};

/// fenster pro batch. die exportierten graphs nehmen bis 32; acht halten den spitzenspeicher klein.
pub(crate) const BATCH: usize = 8;
/// unter so viel sauberer sprache ist ein embedding zu verrauscht.
pub(crate) const MIN_CLEAN_MS: u64 = 300;
pub(crate) const EMBED_DIM: usize = 256;

pub(crate) fn embed_batch(
    fbank: &Model,
    embedding: &Model,
    samples: &[f32],
    start_sample: usize,
    masks: &[LocalMask],
) -> Result<Vec<Option<Vec<f32>>>, Error> {
    let selected = masks
        .iter()
        .enumerate()
        .filter(|(_, mask)| clean_ms(mask) >= MIN_CLEAN_MS)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let mut out = vec![None; masks.len()];
    if selected.is_empty() {
        return Ok(out);
    }

    let count = selected.len();
    let audio = Tensor::zeros_f32(&[count, 1, WINDOW_SAMPLES])?;
    let end = (start_sample + WINDOW_SAMPLES).min(samples.len());
    if start_sample < end {
        for row in 0..count {
            audio.write_f32_at(row * WINDOW_SAMPLES, &samples[start_sample..end]);
        }
    }

    let features = fbank.predict(&[("audio", &audio)])?.get("fbank_features")?;
    let weights = Tensor::zeros_f32(&[count, FRAMES])?;
    for (row, &local) in selected.iter().enumerate() {
        let mask = &masks[local];
        let values = (0..FRAMES)
            .map(|frame| {
                let active = mask.activity.get(frame).copied().unwrap_or(0.0) > 0.5;
                let overlap = mask.overlap.get(frame).copied().unwrap_or(false);
                if active && !overlap { 1.0 } else { 0.0 }
            })
            .collect::<Vec<_>>();
        weights.write_f32_at(row * FRAMES, &values);
    }

    let output = embedding.predict(&[("fbank_features", &features), ("weights", &weights)])?;
    let data = output.get("embedding")?.read_f32();
    if data.len() < count * EMBED_DIM {
        return Err(Error::CoreMl(format!(
            "embedding: expected {} values, got {}",
            count * EMBED_DIM,
            data.len()
        )));
    }
    for (row, &local) in selected.iter().enumerate() {
        let mut vector = data[row * EMBED_DIM..(row + 1) * EMBED_DIM].to_vec();
        cluster::normalize(&mut vector);
        out[local] = Some(vector);
    }
    Ok(out)
}

/// wrapper für fenster, deren sprecher zu kurz für ein cluster-mitglied war, aber lang genug, um ihn dem nächsten centroid zuzuordnen.
pub(crate) fn embed_one(
    fbank: &Model,
    embedding: &Model,
    samples: &[f32],
    start_sample: usize,
    mask: &LocalMask,
) -> Result<Option<Vec<f32>>, Error> {
    if clean_ms(mask) < MIN_CLEAN_MS {
        return Ok(None);
    }
    let mut out = embed_batch(fbank, embedding, samples, start_sample, std::slice::from_ref(mask))?;
    Ok(out.pop().flatten())
}

#[cfg(test)]
pub(crate) const FBANK_FRAMES: usize = 998;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_rate_is_consistent() {
        assert_eq!(crate::segment::SAMPLE_RATE as usize * 10, WINDOW_SAMPLES);
        assert_eq!(FRAMES, 589);
        assert_eq!(FBANK_FRAMES, 998);
    }
}
