
use std::collections::{BTreeMap, HashMap};

use crate::cluster::{self, Embedding};
use crate::segment::{FRAME_MS, SAMPLE_RATE, WindowMask};

const MIN_SPEECH_MS: u64 = 250;
const MIN_TURN_MS: u64 = 300;
const GAP_FILL_MS: u64 = 300;
const PAD_MS: u64 = 100;
const MAX_TURN_MS: u64 = 25_000;
const SPLIT_WINDOW_MS: u64 = 32;

#[derive(Clone, Debug)]
pub struct Turn {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker: usize,
    pub overlap: bool,
    /// das eigene gemittelte embedding des turns. `None` bei turns unter 1 s.
    pub embedding: Option<Vec<f32>>,
}

#[derive(Clone, Copy, Debug)]
struct Run {
    speaker: Option<usize>,
    start_ms: u64,
    end_ms: u64,
    overlap: bool,
}

fn frame_time(frame: usize) -> u64 {
    (frame as f32 * FRAME_MS).round() as u64
}

/// mittelt die aktivität aller überlappenden fenster pro globalem sprecher und beschriftet jeden frame mit dem stärksten sprecher über 0,5.
pub fn labels(
    windows: &[usize],
    masks: &[WindowMask],
    speaker_of: &HashMap<(usize, usize), usize>,
    _total_samples: usize,
    total_ms: u64,
) -> (Vec<Option<usize>>, Vec<bool>) {
    let frames = (total_ms as f64 / FRAME_MS as f64).ceil() as usize;
    if frames == 0 {
        return (Vec::new(), Vec::new());
    }
    let samples_per_frame = SAMPLE_RATE as f64 * FRAME_MS as f64 / 1_000.0;
    let mut coverage = vec![0u32; frames];
    let mut sums: BTreeMap<usize, Vec<f32>> = BTreeMap::new();

    for (index, start) in windows.iter().enumerate() {
        let Some(mask) = masks.get(index) else {
            continue;
        };
        let first = (*start as f64 / samples_per_frame).round() as usize;
        for (local, mask) in mask.masks.iter().enumerate() {
            let Some(&speaker) = speaker_of.get(&(index, local)) else {
                continue;
            };
            let sum = sums.entry(speaker).or_insert_with(|| vec![0.0; frames]);
            for (offset, value) in mask.activity.iter().enumerate() {
                let frame = first + offset;
                if frame >= frames {
                    break;
                }
                if *value > 0.5 {
                    sum[frame] += *value;
                }
            }
        }
        for cell in coverage.iter_mut().take((first + 589).min(frames)).skip(first) {
            *cell += 1;
        }
    }

    let mut out = vec![None; frames];
    let mut overlaps = vec![false; frames];
    // aktivität ist binär, gleichzeitige sprecher liegen also gleichauf. wer schon sprach, behält den frame; sonst gewinnt der kleinste index.
    let mut previous: Option<usize> = None;
    for (frame, &coverage) in coverage.iter().enumerate() {
        let coverage = coverage.max(1) as f32;
        let mut best: Option<(usize, f32)> = None;
        let mut active = 0;
        for (&speaker, sum) in &sums {
            let value = sum[frame] / coverage;
            if value > 0.5 {
                active += 1;
                let wins = match best {
                    None => true,
                    Some((_, current)) if value > current + f32::EPSILON => true,
                    Some((held, current)) => {
                        (value - current).abs() <= f32::EPSILON
                            && previous == Some(speaker)
                            && held != speaker
                    }
                };
                if wins {
                    best = Some((speaker, value));
                }
            }
        }
        if let Some((speaker, _)) = best {
            out[frame] = Some(speaker);
            overlaps[frame] = active > 1;
        }
        previous = best.map(|(speaker, _)| speaker);
    }
    (out, overlaps)
}

pub fn reconstruct(labels: &[Option<usize>], overlaps: &[bool], total_ms: u64) -> Vec<Turn> {
    let frames = labels.len();
    if frames == 0 {
        return Vec::new();
    }
    let mut runs: Vec<Run> = Vec::new();
    for (frame, &speaker) in labels.iter().enumerate().take(frames) {
        let start = frame_time(frame);
        let end = frame_time(frame + 1).min(total_ms);
        if end <= start {
            continue;
        }
        let overlap = overlaps.get(frame).copied().unwrap_or(false);
        match runs.last_mut() {
            Some(run) if run.speaker == speaker => {
                run.end_ms = end;
                run.overlap |= overlap;
            }
            _ => runs.push(Run { speaker, start_ms: start, end_ms: end, overlap }),
        }
    }

    // kurze pausen zwischen demselben sprecher füllen.
    let mut index = 0;
    while index + 2 < runs.len() {
        let gap = runs[index + 1].end_ms - runs[index + 1].start_ms;
        if runs[index].speaker.is_some()
            && runs[index + 1].speaker.is_none()
            && runs[index + 2].speaker == runs[index].speaker
            && gap < GAP_FILL_MS
        {
            runs[index].end_ms = runs[index + 2].end_ms;
            runs[index].overlap |= runs[index + 2].overlap;
            runs.drain(index + 1..index + 3);
        } else {
            index += 1;
        }
    }

    // sehr kurze sprach-runs verwerfen, den rest ohne überlappung polstern.
    let speech = runs
        .into_iter()
        .filter(|run| run.speaker.is_some() && run.end_ms - run.start_ms >= MIN_SPEECH_MS)
        .collect::<Vec<_>>();
    let mut turns: Vec<Turn> = Vec::new();
    for run in speech {
        let Some(speaker) = run.speaker else {
            continue;
        };
        let previous_end = turns.last().map(|turn| turn.end_ms).unwrap_or(0);
        let start_ms = run.start_ms.saturating_sub(PAD_MS).max(previous_end);
        let end_ms = (run.end_ms + PAD_MS).min(total_ms).max(start_ms);
        if end_ms <= start_ms {
            continue;
        }
        if let Some(turn) = turns.last_mut()
            && turn.speaker == speaker
        {
            turn.end_ms = end_ms;
            turn.overlap |= run.overlap;
            continue;
        }
        turns.push(Turn { start_ms, end_ms, speaker, overlap: run.overlap, embedding: None });
    }
    turns.retain(|turn| turn.end_ms - turn.start_ms >= MIN_TURN_MS);
    turns
}

/// teilt turns über 25 s am leisesten 32-ms-frame nahe der grenze.
pub fn split_long(turns: &mut Vec<Turn>, samples: &[f32]) {
    let mut index = 0;
    while index < turns.len() {
        if turns[index].end_ms - turns[index].start_ms <= MAX_TURN_MS {
            index += 1;
            continue;
        }
        let turn = turns[index].clone();
        let split = quietest_split(samples, turn.start_ms, turn.end_ms);
        if split <= turn.start_ms || split >= turn.end_ms {
            index += 1;
            continue;
        }
        turns[index].end_ms = split;
        turns.insert(
            index + 1,
            Turn {
                start_ms: split,
                end_ms: turn.end_ms,
                speaker: turn.speaker,
                overlap: false,
                embedding: None,
            },
        );
        index += 1;
    }
}

fn quietest_split(samples: &[f32], start_ms: u64, end_ms: u64) -> u64 {
    let low = (start_ms + MAX_TURN_MS - 3_000).max(start_ms + 1);
    let high = (start_ms + MAX_TURN_MS).min(end_ms.saturating_sub(1));
    if high <= low {
        return start_ms + MAX_TURN_MS;
    }
    let step = SAMPLE_RATE as u64 * SPLIT_WINDOW_MS / 1_000;
    let to_sample = |ms: u64| (ms * SAMPLE_RATE as u64 / 1_000) as usize;
    let mut best = low;
    let mut best_energy = f32::INFINITY;
    let mut ms = low;
    while ms < high {
        let begin = to_sample(ms).min(samples.len());
        let end = (to_sample(ms + SPLIT_WINDOW_MS)).min(samples.len());
        if begin >= end {
            break;
        }
        let energy = samples[begin..end].iter().map(|value| value * value).sum::<f32>();
        if energy < best_energy {
            best_energy = energy;
            best = ms;
        }
        ms += step.max(1);
    }
    best
}

pub fn attach_embeddings(
    turns: &mut [Turn],
    embeddings: &[Embedding],
    assignment: &[usize],
    _centroids: &[Vec<f32>],
) {
    const WINDOW_MS: u64 = 10_000;
    for turn in turns {
        if turn.end_ms - turn.start_ms < 1_000 {
            turn.embedding = None;
            continue;
        }
        let mut members = Vec::new();
        for (embedding, &cluster) in embeddings.iter().zip(assignment) {
            if cluster != turn.speaker {
                continue;
            }
            let start = embedding.start_ms;
            let end = start + WINDOW_MS;
            let overlap = end.min(turn.end_ms).saturating_sub(start.max(turn.start_ms));
            if overlap >= 300 {
                members.push(embedding.vector.as_slice());
            }
        }
        turn.embedding = if members.is_empty() { None } else { Some(cluster::mean(&members)) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels_for(speaker: Option<usize>, frames: usize) -> Vec<Option<usize>> {
        vec![speaker; frames]
    }

    #[test]
    fn one_speech_run_becomes_one_padded_turn() {
        let frames = 118; // ~2 s
        let labels = labels_for(Some(0), frames);
        let overlaps = vec![false; frames];
        let turns = reconstruct(&labels, &overlaps, 4_000);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].speaker, 0);
        assert!(turns[0].start_ms < 100);
        assert!(turns[0].end_ms > 1_900);
    }

    #[test]
    fn short_silence_between_the_same_speaker_is_filled() {
        let mut labels = vec![Some(0); 60];
        labels.extend(std::iter::repeat_n(None, 6)); // ~100 ms pause
        labels.extend(std::iter::repeat_n(Some(0), 60));
        let overlaps = vec![false; labels.len()];
        let turns = reconstruct(&labels, &overlaps, 3_000);
        assert_eq!(turns.len(), 1);
    }

    #[test]
    fn a_different_speaker_is_not_filled() {
        let mut labels = vec![Some(0); 60];
        labels.extend(std::iter::repeat_n(None, 6));
        labels.extend(std::iter::repeat_n(Some(1), 60));
        let overlaps = vec![false; labels.len()];
        let turns = reconstruct(&labels, &overlaps, 3_000);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].end_ms, turns[1].start_ms);
    }

    #[test]
    fn very_short_runs_are_dropped() {
        let mut labels = vec![Some(0); 60];
        labels.extend(std::iter::repeat_n(None, 60));
        labels.extend(std::iter::repeat_n(Some(1), 4)); // ~68 ms
        labels.extend(std::iter::repeat_n(None, 60));
        let overlaps = vec![false; labels.len()];
        let turns = reconstruct(&labels, &overlaps, 3_000);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].speaker, 0);
    }

    #[test]
    fn long_turns_are_split_at_quiet_samples() {
        let mut samples = vec![0.5f32; 16_000 * 30];
        // die letzten drei sekunden vor 25 s fast stumm machen.
        for sample in &mut samples[16_000 * 23..16_000 * 24] {
            *sample = 0.0;
        }
        let mut turns =
            vec![Turn { start_ms: 0, end_ms: 30_000, speaker: 0, overlap: false, embedding: None }];
        split_long(&mut turns, &samples);
        assert!(turns.len() >= 2);
        assert!(turns[0].end_ms <= 25_000);
        assert!(turns[0].end_ms >= 22_000);
    }

    #[test]
    fn simultaneous_speakers_resolve_to_the_one_already_talking() {
        let mask =
            |activity: Vec<f32>| crate::segment::LocalMask { overlap: vec![false; activity.len()], activity };
        let mut a = vec![0.0; 589];
        let mut b = vec![0.0; 589];
        a[..300].fill(1.0);
        b[100..200].fill(1.0);
        let masks = vec![crate::segment::WindowMask { masks: vec![mask(a), mask(b)] }];
        let speaker_of = HashMap::from([((0, 0), 5), ((0, 1), 2)]);
        let (labels, overlaps) = labels(&[0], &masks, &speaker_of, 160_000, 10_000);
        assert_eq!(labels[150], Some(5));
        assert!(overlaps[150]);
        assert_eq!(labels[250], Some(5));
    }

    #[test]
    fn overlap_is_kept_on_the_turn() {
        let labels = labels_for(Some(0), 60);
        let mut overlaps = vec![false; 60];
        overlaps[10] = true;
        let turns = reconstruct(&labels, &overlaps, 2_000);
        assert!(turns[0].overlap);
    }
}
