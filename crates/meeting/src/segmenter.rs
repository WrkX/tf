//! silero vad auf coreml ist geplant; das interface bleibt dabei gleich, frame-vertrag, damit das modell [`Segmenter::frame_db`] ersetzen kann, ohne den capture-pfad anzufassen. frames sind 32 ms, sprache beginnt nach zwei lauten frames und schließt nach 600 ms ruhe, mit 200 ms padding.

use std::collections::VecDeque;

const FRAME_MS: u64 = 32;
const START_FRAMES: u32 = 2;
const END_FRAMES: u32 = 19;
const PAD_MS: u64 = 200;
const MAX_SEGMENT_MS: u64 = 25_000;
const MIN_SEGMENT_MS: u64 = 200;
const VOICED_MEMORY_MS: u64 = 60_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Speaker {
    Me,
    Them,
}

impl Speaker {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Me => "me",
            Self::Them => "them",
        }
    }

    pub fn parse(value: &str) -> Self {
        if value.eq_ignore_ascii_case("me") { Self::Me } else { Self::Them }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Me => "Me",
            Self::Them => "Them",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ClosedSegment {
    pub speaker: Speaker,
    pub start_ms: u64,
    pub end_ms: u64,
    pub samples: Vec<f32>,
}

pub struct Segmenter {
    speaker: Speaker,
    rate: u32,
    frame_len: usize,
    pending: Vec<f32>,
    pre: VecDeque<f32>,
    current: Vec<f32>,
    noise_db: f32,
    speech_run: u32,
    silence_run: u32,
    active: bool,
    start_ms: u64,
    frame_ms: u64,
    origin_ms: Option<u64>,
    speech_seen: bool,
    voiced: VecDeque<(u64, u64)>,
}

impl Segmenter {
    pub fn new(speaker: Speaker, rate: u32) -> Self {
        let rate = rate.max(8_000);
        Self {
            speaker,
            rate,
            frame_len: (rate as u64 * FRAME_MS / 1000) as usize,
            pending: Vec::new(),
            pre: VecDeque::new(),
            current: Vec::new(),
            noise_db: -60.0,
            speech_run: 0,
            silence_run: 0,
            active: false,
            start_ms: 0,
            frame_ms: 0,
            origin_ms: None,
            speech_seen: false,
            voiced: VecDeque::new(),
        }
    }

    /// millisekunden von `start_ms..end_ms`, in denen diese seite laut genug für sprache war. nur die letzte minute wird behalten.
    pub fn voiced_overlap_ms(&self, start_ms: u64, end_ms: u64) -> u64 {
        self.voiced.iter().map(|(from, to)| end_ms.min(*to).saturating_sub(start_ms.max(*from))).sum()
    }

    fn record_voiced(&mut self, start_ms: u64) {
        let end_ms = start_ms + FRAME_MS;
        match self.voiced.back_mut() {
            Some(last) if start_ms <= last.1 => last.1 = end_ms,
            _ => self.voiced.push_back((start_ms, end_ms)),
        }
        while self.voiced.front().is_some_and(|span| span.1 + VOICED_MEMORY_MS < end_ms) {
            self.voiced.pop_front();
        }
    }

    pub fn speaker(&self) -> Speaker {
        self.speaker
    }

    /// schiebt einen block durch und gibt jedes darin geschlossene segment zurück. `start_ms` ist der wall-clock-offset des ersten samples.
    pub fn push(&mut self, samples: &[f32], start_ms: u64) -> Vec<ClosedSegment> {
        let origin = *self.origin_ms.get_or_insert(start_ms);
        self.pending.extend_from_slice(samples);
        let mut closed = Vec::new();
        while self.pending.len() >= self.frame_len {
            let frame = self.pending.drain(..self.frame_len).collect::<Vec<_>>();
            let frame_start = origin + self.frame_ms;
            self.frame_ms += FRAME_MS;
            let speech = self.frame_db(&frame) > self.threshold();
            if speech {
                self.record_voiced(frame_start);
            }
            if let Some(segment) = self.frame(frame, frame_start, speech) {
                closed.push(segment);
            }
        }
        closed
    }

    /// schließt das offene segment, falls es eines gibt, am ende des gepushten audios.
    pub fn flush(&mut self) -> Option<ClosedSegment> {
        if !self.active {
            return None;
        }
        let duration = self.current.len() as u64 * 1000 / u64::from(self.rate);
        if duration < MIN_SEGMENT_MS || !self.speech_seen {
            self.reset();
            return None;
        }
        let end_ms = self.start_ms + duration;
        let segment = ClosedSegment {
            speaker: self.speaker,
            start_ms: self.start_ms,
            end_ms,
            samples: std::mem::take(&mut self.current),
        };
        self.reset();
        Some(segment)
    }

    fn frame(&mut self, frame: Vec<f32>, frame_start: u64, speech: bool) -> Option<ClosedSegment> {
        if !self.active {
            for sample in &frame {
                self.pre.push_back(*sample);
            }
            let keep = (self.rate as u64 * PAD_MS / 1000) as usize;
            while self.pre.len() > keep {
                self.pre.pop_front();
            }
            if speech {
                self.speech_run += 1;
                if self.speech_run >= START_FRAMES {
                    self.active = true;
                    self.silence_run = 0;
                    self.speech_seen = true;
                    self.current = self.pre.drain(..).collect();
                    self.start_ms =
                        frame_start.saturating_sub(self.current.len() as u64 * 1000 / u64::from(self.rate));
                    return None;
                }
            } else {
                self.speech_run = 0;
                if self.noise_db < -20.0 {
                    self.noise_db = self.noise_db * 0.95 + self.frame_energy(&frame) * 0.05;
                }
            }
            return None;
        }

        self.current.extend_from_slice(&frame);
        if speech {
            self.silence_run = 0;
            self.speech_seen = true;
        } else {
            self.silence_run += 1;
        }

        let duration = self.current.len() as u64 * 1000 / u64::from(self.rate);
        if duration >= MAX_SEGMENT_MS {
            let end_ms = self.start_ms + duration;
            let segment = ClosedSegment {
                speaker: self.speaker,
                start_ms: self.start_ms,
                end_ms,
                samples: std::mem::take(&mut self.current),
            };
            self.start_ms = end_ms;
            self.silence_run = 0;
            self.speech_seen = false;
            return Some(segment);
        }

        if self.silence_run >= END_FRAMES {
            let trim = (self.rate as u64 * (END_FRAMES as u64 * FRAME_MS - PAD_MS) / 1000) as usize;
            let keep = self.current.len().saturating_sub(trim);
            self.current.truncate(keep);
            let duration = self.current.len() as u64 * 1000 / u64::from(self.rate);
            if duration < MIN_SEGMENT_MS || !self.speech_seen {
                self.reset();
                return None;
            }
            let segment = ClosedSegment {
                speaker: self.speaker,
                start_ms: self.start_ms,
                end_ms: self.start_ms + duration,
                samples: std::mem::take(&mut self.current),
            };
            self.reset();
            return Some(segment);
        }
        None
    }

    fn reset(&mut self) {
        self.active = false;
        self.speech_run = 0;
        self.silence_run = 0;
        self.speech_seen = false;
        self.current.clear();
        self.pre.clear();
    }

    fn threshold(&self) -> f32 {
        (self.noise_db + 9.0).clamp(-52.0, -24.0)
    }

    fn frame_db(&self, frame: &[f32]) -> f32 {
        self.frame_energy(frame)
    }

    fn frame_energy(&self, frame: &[f32]) -> f32 {
        if frame.is_empty() {
            return -120.0;
        }
        let rms = (frame.iter().map(|sample| sample * sample).sum::<f32>() / frame.len() as f32).sqrt();
        20.0 * rms.max(1e-7).log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    const RATE: u32 = 16_000;

    fn tone(secs: f64, amplitude: f32) -> Vec<f32> {
        let n = (RATE as f64 * secs) as usize;
        (0..n).map(|i| amplitude * (TAU * 440.0 * i as f64 / RATE as f64).sin() as f32).collect()
    }

    fn silence(secs: f64) -> Vec<f32> {
        vec![0.0; (RATE as f64 * secs) as usize]
    }

    #[test]
    fn silence_never_closes_a_segment() {
        let mut segmenter = Segmenter::new(Speaker::Them, RATE);
        assert!(segmenter.push(&silence(2.0), 0).is_empty());
        assert!(segmenter.flush().is_none());
    }

    #[test]
    fn speech_closes_after_six_hundred_milliseconds_of_silence() {
        let mut segmenter = Segmenter::new(Speaker::Me, RATE);
        let mut audio = silence(0.2);
        audio.extend(tone(1.0, 0.25));
        audio.extend(silence(1.0));
        let closed = segmenter.push(&audio, 0);
        assert_eq!(closed.len(), 1);
        let segment = &closed[0];
        assert_eq!(segment.speaker, Speaker::Me);
        assert!((segment.start_ms as i64).abs() <= 250, "start {}", segment.start_ms);
        let duration = segment.samples.len() as f64 / RATE as f64;
        assert!((1.0..=1.5).contains(&duration), "duration {duration}");
    }

    #[test]
    fn voiced_overlap_reports_loud_stretches_only() {
        let mut segmenter = Segmenter::new(Speaker::Them, RATE);
        let mut audio = silence(1.0);
        audio.extend(tone(1.0, 0.25));
        audio.extend(silence(1.0));
        segmenter.push(&audio, 0);
        assert!(segmenter.voiced_overlap_ms(0, 900) < 100);
        assert!(segmenter.voiced_overlap_ms(1_000, 2_000) > 800);
    }

    #[test]
    fn streaming_blocks_match_one_shot_input() {
        let audio = {
            let mut audio = silence(0.2);
            audio.extend(tone(0.8, 0.25));
            audio.extend(silence(1.0));
            audio
        };
        let mut one = Segmenter::new(Speaker::Them, RATE);
        let expected = one.push(&audio, 0);
        let mut many = Segmenter::new(Speaker::Them, RATE);
        let mut actual = Vec::new();
        for (index, block) in audio.chunks(1_600).enumerate() {
            actual.extend(many.push(block, index as u64 * 100));
        }
        assert_eq!(actual.len(), expected.len());
        assert_eq!(actual[0].start_ms, expected[0].start_ms);
        assert_eq!(actual[0].end_ms, expected[0].end_ms);
    }

    #[test]
    fn very_long_speech_is_force_split() {
        let mut segmenter = Segmenter::new(Speaker::Them, RATE);
        let closed = segmenter.push(&tone(30.0, 0.3), 0);
        assert!(!closed.is_empty());
        assert!(closed[0].end_ms - closed[0].start_ms <= 25_500);
    }

    #[test]
    fn flush_returns_the_open_segment() {
        let mut segmenter = Segmenter::new(Speaker::Me, RATE);
        assert!(segmenter.push(&tone(0.6, 0.3), 0).is_empty());
        let segment = segmenter.flush().expect("open segment");
        assert!(segment.end_ms >= segment.start_ms);
        assert!(!segment.samples.is_empty());
    }
}
