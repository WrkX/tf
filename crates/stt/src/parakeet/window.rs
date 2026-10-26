use std::ops::Range;

pub(crate) const WINDOW_SAMPLES: usize = 240_000;
pub(crate) const SAMPLES_PER_FRAME: usize = 1_280;

const WINDOW_FRAMES: usize = WINDOW_SAMPLES / SAMPLES_PER_FRAME;
const CONTEXT_FRAMES: usize = 25;
const HOP_FRAMES: usize = 135;

/// ein encoder-durchlauf über höchstens 15 s audio. jedes fenster sieht `CONTEXT_FRAMES` audio vor den frames, für die es zuständig ist, und etwas audio danach, damit der encoder kontext auf beiden seiten der grenze hat, jeder frame wird aber nur einmal dekodiert.
#[derive(Debug, PartialEq)]
pub(crate) struct Window {
    pub samples: Range<usize>,
    pub first_frame: usize,
    pub end_frame: usize,
}

pub(crate) fn plan(samples: usize) -> Vec<Window> {
    let total = samples.div_ceil(SAMPLES_PER_FRAME);
    let mut windows = Vec::new();
    let mut from = 0;
    while from < total {
        let first_frame = from.saturating_sub(CONTEXT_FRAMES);
        let capacity = WINDOW_FRAMES - (from - first_frame);
        let end_frame = if total - from <= capacity { total } else { from + HOP_FRAMES };
        let start = first_frame * SAMPLES_PER_FRAME;
        windows.push(Window { samples: start..samples.min(start + WINDOW_SAMPLES), first_frame, end_frame });
        from = end_frame;
    }
    windows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_audio_is_one_window() {
        assert_eq!(plan(16_000 * 5), [Window { samples: 0..80_000, first_frame: 0, end_frame: 63 }]);
    }

    #[test]
    fn long_audio_covers_every_frame_once() {
        let samples = 16_000 * 61;
        let windows = plan(samples);
        let mut next = 0;
        for w in &windows {
            assert!(w.samples.len() <= WINDOW_SAMPLES);
            assert!(w.first_frame <= next);
            assert!((w.end_frame - w.first_frame) * SAMPLES_PER_FRAME <= WINDOW_SAMPLES + SAMPLES_PER_FRAME);
            next = w.end_frame;
        }
        assert_eq!(next, samples.div_ceil(SAMPLES_PER_FRAME));
    }
}
