use std::f64::consts::TAU;

pub const COUNT: usize = 7;

const PROFILE: [f64; COUNT] = [0.3, 0.55, 0.8, 1.0, 0.8, 0.55, 0.3];
const IDLE_SCALE: f64 = 0.22;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Animation {
    Listening {
        level: f32,
    },
    Processing,
    Error,
    Empty,
}

/// balkenhöhen zwischen `dot` und `max` zum zeitpunkt `t` in sekunden.
pub fn heights(animation: Animation, t: f64, dot: f64, max: f64) -> [f64; COUNT] {
    let range = max - dot;
    std::array::from_fn(|i| {
        let i_f = i as f64;
        let scale = match animation {
            Animation::Listening { level } => {
                let wobble = 0.85 + 0.15 * (t * 9.0 + i_f * 1.7).sin();
                PROFILE[i] * (IDLE_SCALE + (1.0 - IDLE_SCALE) * level as f64 * wobble)
            }
            Animation::Processing => {
                let wave = (t * 1.6 * TAU - i_f * 0.75).sin().max(0.0);
                0.35 * wave * wave
            }
            Animation::Error | Animation::Empty => PROFILE[i] * IDLE_SCALE,
        };
        dot + range * scale
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heights_stay_in_bounds() {
        for t in 0..200 {
            let t = t as f64 * 0.017;
            for a in [
                Animation::Listening { level: 0.0 },
                Animation::Listening { level: 1.0 },
                Animation::Processing,
                Animation::Error,
                Animation::Empty,
            ] {
                for h in heights(a, t, 3.0, 18.0) {
                    assert!((3.0..=18.0).contains(&h), "{h}");
                }
            }
        }
    }

    #[test]
    fn silence_keeps_center_tallest() {
        let h = heights(Animation::Listening { level: 0.0 }, 0.0, 3.0, 18.0);
        assert!(h[3] > h[2] && h[2] > h[1] && h[1] > h[0]);
    }
}
