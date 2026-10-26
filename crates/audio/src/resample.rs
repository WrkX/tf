
use std::f64::consts::TAU;

/// taps auf jeder seite des interpolationspunkts.
const HALF_TAPS: usize = 32;
/// kaiser-form; ungefähr 80 db sperrdämpfung.
const BETA: f64 = 8.6;

pub const TARGET_RATE: u32 = 16_000;

pub fn to_16k(samples: &[f32], from_hz: u32) -> Vec<f32> {
    if from_hz == TARGET_RATE {
        return samples.to_vec();
    }
    resample(samples, from_hz, TARGET_RATE)
}

/// rationales oder irrationales verhältnis; die ausgabe ist `round(n * to / from)` lang.
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if input.is_empty() || from == 0 || to == 0 {
        return Vec::new();
    }
    if from == to {
        return input.to_vec();
    }

    let out_len = (input.len() as f64 * to as f64 / from as f64).round() as usize;
    // TODO: sinc-kerne pro verhältnis cachen; passband unter der niedrigeren nyquist-frequenz halten, bei 45 % davon.
    let cutoff = 0.45 * f64::from(from.min(to)) / f64::from(from);
    let half = HALF_TAPS as f64;
    let i0_beta = bessel_i0(BETA);
    let step = f64::from(from) / f64::from(to);
    let last = input.len() as i64 - 1;

    let mut out = Vec::with_capacity(out_len);
    for n in 0..out_len {
        let center = n as f64 * step;
        let lo = (center - half).ceil() as i64;
        let hi = (center + half).floor() as i64;
        let mut acc = 0.0;
        let mut weight = 0.0;
        for k in lo..=hi {
            if k < 0 || k > last {
                continue;
            }
            let x = center - k as f64;
            let w = kernel(x, cutoff, half, i0_beta);
            acc += f64::from(input[k as usize]) * w;
            weight += w;
        }
        out.push(if weight.abs() > 1e-12 { (acc / weight) as f32 } else { 0.0 });
    }
    out
}

fn kernel(x: f64, cutoff: f64, half: f64, i0_beta: f64) -> f64 {
    if x.abs() > half {
        return 0.0;
    }
    let arg = TAU * cutoff * x;
    let sinc = if arg.abs() < 1e-9 { 1.0 } else { arg.sin() / arg };
    let r = x / half;
    sinc * bessel_i0(BETA * (1.0 - r * r).max(0.0).sqrt()) / i0_beta
}

fn bessel_i0(x: f64) -> f64 {
    let y = x * x / 4.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    for k in 1..64 {
        term *= y / f64::from(k * k);
        sum += term;
        if term < sum * 1e-12 {
            break;
        }
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, hz: f64, secs: f64, amplitude: f32) -> Vec<f32> {
        let n = (rate as f64 * secs) as usize;
        (0..n).map(|i| amplitude * (TAU * hz * i as f64 / rate as f64).sin() as f32).collect()
    }

    fn rms(samples: &[f32]) -> f64 {
        (samples.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
    }

    #[test]
    fn pass_through_at_16k() {
        let input = sine(16_000, 440.0, 0.1, 0.5);
        assert_eq!(to_16k(&input, 16_000), input);
    }

    #[test]
    fn output_length_is_rounded_ratio() {
        for (from, n) in [(48_000, 48_000), (44_100, 44_100), (8_000, 8_000)] {
            let out = to_16k(&vec![0.0; n], from);
            assert_eq!(out.len(), (n as f64 * 16_000.0 / from as f64).round() as usize);
        }
    }

    #[test]
    fn one_khz_keeps_amplitude() {
        for from in [48_000, 44_100] {
            let input = sine(from, 1_000.0, 0.5, 0.5);
            let out = to_16k(&input, from);
            let edge = 400;
            let gain_db = 20.0 * (rms(&out[edge..out.len() - edge]) / rms(&input[edge..])).log10();
            assert!(gain_db.abs() < 0.1, "{from} Hz input: {gain_db} dB");
        }
    }

    #[test]
    fn ten_khz_is_rejected() {
        for from in [48_000, 44_100] {
            let input = sine(from, 10_000.0, 0.5, 0.5);
            let out = to_16k(&input, from);
            let edge = 400;
            let gain_db = 20.0 * (rms(&out[edge..out.len() - edge]) / rms(&input[edge..])).log10();
            assert!(gain_db < -60.0, "{from} Hz input: {gain_db} dB");
        }
    }

    #[test]
    fn empty_and_zero_rate() {
        assert!(to_16k(&[], 48_000).is_empty());
        assert!(resample(&[0.0; 10], 0, 16_000).is_empty());
    }
}
