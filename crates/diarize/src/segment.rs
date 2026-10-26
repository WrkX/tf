
pub const SAMPLE_RATE: u32 = 16_000;
pub const WINDOW_SAMPLES: usize = 160_000;
/// pyannote segmentation-3.0 liefert 589 frames pro 10-s-fenster.
pub const FRAMES: usize = 589;
pub const CLASSES: usize = 7;
/// dauer eines segmentierungs-frames in millisekunden.
pub const FRAME_MS: f32 = 10_000.0 / FRAMES as f32;

#[derive(Clone, Debug, Default)]
pub struct LocalMask {
    /// ein wert pro segmentierungs-frame; 1.0 solange der sprecher aktiv ist.
    pub activity: Vec<f32>,
    /// frames, in denen noch ein anderer lokaler sprecher aktiv ist.
    pub overlap: Vec<bool>,
}

#[derive(Clone, Debug, Default)]
pub struct WindowMask {
    pub masks: Vec<LocalMask>,
}

pub fn plan(samples: usize, step: usize) -> Vec<usize> {
    if samples == 0 {
        return Vec::new();
    }
    let step = step.max(1);
    let mut starts = Vec::new();
    let mut start = 0;
    while start < samples {
        starts.push(start);
        start = start.saturating_add(step);
    }
    starts
}

pub const LOCAL_SPEAKERS: usize = 3;

/// welche lokalen sprecher in jeder der sieben powerset-klassen aktiv sind.
pub fn class_speakers(class: usize) -> &'static [usize] {
    const CLASS_MAP: [&[usize]; 7] = [&[], &[0], &[1], &[2], &[0, 1], &[0, 2], &[1, 2]];
    CLASS_MAP.get(class).copied().unwrap_or(&[])
}

/// dekodiert `[589, 7]`-log-probabilities in eine maske pro lokalem sprecher.
pub fn decode(log_probs: &[f32]) -> WindowMask {
    let frames = log_probs.len() / CLASSES;
    let mut masks = vec![LocalMask::default(); LOCAL_SPEAKERS];
    for mask in &mut masks {
        mask.activity = vec![0.0; frames];
        mask.overlap = vec![false; frames];
    }
    for frame in 0..frames {
        let base = frame * CLASSES;
        let mut best = 0usize;
        let mut best_value = f32::NEG_INFINITY;
        for class in 0..CLASSES {
            let value = log_probs[base + class];
            if value > best_value {
                best_value = value;
                best = class;
            }
        }
        let active = class_speakers(best);
        for &local in active {
            if let Some(mask) = masks.get_mut(local) {
                mask.activity[frame] = 1.0;
                mask.overlap[frame] = active.len() > 1;
            }
        }
    }
    WindowMask { masks }
}

/// dauer sauberer (nicht überlappter) sprache einer lokalen maske in millisekunden.
pub fn clean_ms(mask: &LocalMask) -> u64 {
    let frames = mask
        .activity
        .iter()
        .zip(&mask.overlap)
        .filter(|(activity, overlap)| **activity > 0.5 && !**overlap)
        .count();
    (frames as f32 * FRAME_MS).round() as u64
}

pub fn activity_ms(mask: &LocalMask) -> u64 {
    let frames = mask.activity.iter().filter(|value| **value > 0.5).count();
    (frames as f32 * FRAME_MS).round() as u64
}

pub fn has_speech(mask: &WindowMask) -> bool {
    mask.masks.iter().any(|mask| activity_ms(mask) > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log_probs(classes: &[usize]) -> Vec<f32> {
        classes
            .iter()
            .flat_map(|&class| (0..CLASSES).map(move |index| if index == class { 0.0 } else { -10.0 }))
            .collect()
    }

    #[test]
    fn plans_overlapping_windows_with_a_padded_tail() {
        assert_eq!(plan(10_000, 40_000), vec![0]);
        assert_eq!(plan(200_000, 40_000), vec![0, 40_000, 80_000, 120_000, 160_000]);
    }

    #[test]
    fn decodes_every_class() {
        // ein frame pro klasse: stille, drei einzelne, drei paare.
        let classes = [0, 1, 2, 3, 4, 5, 6];
        let mask = decode(&log_probs(&classes));
        assert_eq!(mask.masks.len(), 3);
        assert_eq!(mask.masks[0].activity, vec![0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0]);
        assert_eq!(mask.masks[1].activity, vec![0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0]);
        assert_eq!(mask.masks[2].activity, vec![0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 1.0]);
        assert_eq!(mask.masks[0].overlap, vec![false, false, false, false, true, true, false]);
    }

    #[test]
    fn clean_duration_excludes_overlaps() {
        let mut mask = LocalMask { activity: vec![1.0; 59], overlap: vec![false; 59] };
        let full = activity_ms(&mask);
        assert!((full as i64 - 1_002).abs() <= 2);
        assert_eq!(clean_ms(&mask), full);
        mask.overlap[0] = true;
        assert!(clean_ms(&mask) < full);
    }
}
