
use serde::{Deserialize, Serialize};

pub const FRAME_MS: u32 = 30;
pub const HANGOVER_MS: u32 = 300;
pub const PADDING_MS: u32 = 250;
const MIN_SPEECH_MS: u32 = 200;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum Sensitivity {
    Low,
    #[default]
    Normal,
    High,
}

impl Sensitivity {
    fn offset_db(self) -> f32 {
        match self {
            Self::Low => 14.0,
            Self::Normal => 10.0,
            Self::High => 7.0,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Trimmed {
    pub samples: Vec<f32>,
    pub speech_ms: u32,
}

pub fn trim(samples: &[f32], rate: u32, sensitivity: Sensitivity) -> Trimmed {
    if samples.is_empty() || rate == 0 {
        return Trimmed::default();
    }
    let frame_len = (rate as usize * FRAME_MS as usize / 1000).max(1);
    let frames = samples.len() / frame_len;
    if frames == 0 {
        return Trimmed::default();
    }

    let energy: Vec<f32> = (0..frames)
        .map(|f| {
            let frame = &samples[f * frame_len..(f + 1) * frame_len];
            let rms = (frame.iter().map(|s| s * s).sum::<f32>() / frame_len as f32).sqrt();
            20.0 * rms.max(1e-10).log10()
        })
        .collect();

    let mut sorted = energy.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let floor = sorted[frames / 10];
    let threshold = (floor + sensitivity.offset_db()).max(-50.0);

    let mut speech: Vec<bool> = energy.iter().map(|&db| db > threshold).collect();
    let hangover = (HANGOVER_MS / FRAME_MS) as usize;
    let mut first = None;
    let mut last = None;
    for (i, &is_speech) in speech.iter().enumerate() {
        if is_speech {
            first.get_or_insert(i);
            last = Some(i);
        }
    }
    let (Some(first), Some(last)) = (first, last) else {
        return Trimmed::default();
    };
    // kurze lücken schließen, damit ein satz nicht an jedem wort abgeschnitten wird.
    for i in first..=last {
        if !speech[i] {
            let gap_end = (i..=last).find(|&j| speech[j]);
            if let Some(gap_end) = gap_end
                && gap_end - i < hangover
            {
                speech[i..gap_end].fill(true);
            }
        }
    }
    let speech_frames = speech.iter().filter(|&&s| s).count();
    let speech_ms = speech_frames as u32 * FRAME_MS;
    if speech_ms < MIN_SPEECH_MS {
        return Trimmed::default();
    }

    let pad = (PADDING_MS / FRAME_MS) as usize;
    let start_frame = first.saturating_sub(pad);
    let end_frame = (last + 1 + pad).min(frames);
    let start = start_frame * frame_len;
    let end = (end_frame * frame_len).min(samples.len());

    Trimmed { samples: samples[start..end].to_vec(), speech_ms }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    const RATE: u32 = 16_000;

    fn silence(secs: f64) -> Vec<f32> {
        vec![0.0; (RATE as f64 * secs) as usize]
    }

    fn tone(secs: f64, amplitude: f32) -> Vec<f32> {
        let n = (RATE as f64 * secs) as usize;
        (0..n).map(|i| amplitude * (TAU * 440.0 * i as f64 / RATE as f64).sin() as f32).collect()
    }

    fn with_noise(mut samples: Vec<f32>, amplitude: f32) -> Vec<f32> {
        let mut seed = 0x1234_5678u32;
        for s in &mut samples {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *s += (seed as f32 / u32::MAX as f32 - 0.5) * 2.0 * amplitude;
        }
        samples
    }

    #[test]
    fn pure_silence_is_empty() {
        assert!(trim(&silence(3.0), RATE, Sensitivity::Normal).samples.is_empty());
    }

    #[test]
    fn trims_burst_with_padding() {
        let mut input = silence(1.0);
        input.extend(tone(1.0, 0.2));
        input.extend(silence(1.0));

        let trimmed = trim(&input, RATE, Sensitivity::Normal);
        assert!(!trimmed.samples.is_empty());
        let secs = trimmed.samples.len() as f64 / RATE as f64;
        assert!((1.2..=1.8).contains(&secs), "{secs}");
        assert!((900..=1050).contains(&trimmed.speech_ms), "{}", trimmed.speech_ms);
    }

    #[test]
    fn burst_in_noise_is_kept() {
        let mut input = with_noise(silence(1.0), 0.001);
        input.extend(tone(0.8, 0.1));
        input.extend(with_noise(silence(1.0), 0.001));
        let trimmed = trim(&input, RATE, Sensitivity::Normal);
        assert!(!trimmed.samples.is_empty());
    }

    #[test]
    fn very_short_burst_is_empty() {
        let mut input = silence(0.5);
        input.extend(tone(0.1, 0.2));
        input.extend(silence(0.5));
        assert!(trim(&input, RATE, Sensitivity::Normal).samples.is_empty());
    }

    #[test]
    fn high_sensitivity_finds_quieter_speech() {
        let mut input = with_noise(silence(0.5), 0.01);
        input.extend(tone(0.8, 0.02));
        input.extend(with_noise(silence(0.5), 0.01));
        assert!(trim(&input, RATE, Sensitivity::Low).samples.is_empty());
        assert!(trim(&input, RATE, Sensitivity::Normal).samples.is_empty());
        assert!(!trim(&input, RATE, Sensitivity::High).samples.is_empty());
    }
}
