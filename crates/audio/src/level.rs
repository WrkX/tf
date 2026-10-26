const FLOOR_DB: f32 = -55.0;
const CEILING_DB: f32 = -12.0;
const ATTACK: f32 = 0.6;
const RELEASE: f32 = 0.12;

pub(crate) fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

const NORMALIZE_TARGET_PEAK: f32 = 0.7;
const NORMALIZE_MAX_GAIN: f32 = 10.0;

/// hebt leises audio an, bis der peak einen angenehmen pegel erreicht. lautes audio bleibt unangetastet, und der gain ist gedeckelt, damit raumrauschen nicht hochgezogen wird.
pub fn normalize(samples: &mut [f32]) {
    let peak = samples.iter().fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    if peak <= f32::EPSILON {
        return;
    }
    let gain = (NORMALIZE_TARGET_PEAK / peak).min(NORMALIZE_MAX_GAIN);
    if gain > 1.0 {
        samples.iter_mut().for_each(|sample| *sample *= gain);
    }
}

/// mappt rohe rms auf eine geglättete, wahrgenommene `0.0..=1.0`-lautheit fürs display.
#[derive(Default)]
pub struct Meter {
    value: f32,
}

impl Meter {
    pub fn update(&mut self, rms: f32) -> f32 {
        let db = 20.0 * rms.max(1e-6).log10();
        let target = ((db - FLOOR_DB) / (CEILING_DB - FLOOR_DB)).clamp(0.0, 1.0);
        let rate = if target > self.value { ATTACK } else { RELEASE };
        self.value += (target - self.value) * rate;
        self.value
    }
}

#[cfg(test)]
mod normalize_tests {
    use super::normalize;

    #[test]
    fn quiet_audio_is_boosted_to_the_target() {
        let mut samples = vec![0.0, 0.14, -0.14];
        normalize(&mut samples);
        assert!((samples[1] - 0.7).abs() < 1e-5);
    }

    #[test]
    fn gain_is_capped_and_loud_audio_is_untouched() {
        let mut faint = vec![0.01];
        normalize(&mut faint);
        assert!((faint[0] - 0.1).abs() < 1e-6);
        let mut loud = vec![0.9, -0.8];
        normalize(&mut loud);
        assert_eq!(loud, vec![0.9, -0.8]);
    }
}
