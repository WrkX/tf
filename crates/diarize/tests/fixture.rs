//! standardmäßig ignoriert: braucht das installierte speaker-identification-modell und eine unterhaltung aus macos-`say`-stimmen. erst `cargo xtask diarize-fixture /tmp/tf-diarize-fixture`, dann `TF_DIARIZE_FIXTURE_DIR=/tmp/tf-diarize-fixture cargo test -p tf-diarize --test fixture -- --ignored --nocapture`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use tf_diarize::{Diarization, Diarizer, Options};

struct Line {
    voice: String,
    start_ms: u64,
    end_ms: u64,
    overlaps_previous: bool,
}

fn models_dir() -> PathBuf {
    std::env::var("TF_MODELS_DIR").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("Library/Application Support/tf/models")
    })
}

fn load_lines(dir: &Path) -> Vec<Line> {
    let text = std::fs::read_to_string(dir.join("fixture.json")).expect("fixture.json");
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    value["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| Line {
            voice: line["voice"].as_str().unwrap().to_owned(),
            start_ms: line["start_ms"].as_u64().unwrap(),
            end_ms: line["end_ms"].as_u64().unwrap(),
            overlaps_previous: line["overlaps_previous"].as_bool().unwrap(),
        })
        .collect()
}

fn diarize(dir: &Path) -> Diarization {
    let samples = tf_audio::file::read(&dir.join("mix.m4a")).expect("read mix.m4a");
    let mut diarizer = Diarizer::load(&models_dir()).expect("load models");
    let mut options = Options::default();
    if let Some(threshold) = std::env::var("TF_DIARIZE_THRESHOLD").ok().and_then(|value| value.parse().ok()) {
        options.threshold = threshold;
    }
    diarizer.run(&samples, &options, |_| {}, &AtomicBool::new(false)).expect("diarize")
}

fn dominant_speaker(result: &Diarization, line: &Line) -> Option<usize> {
    let margin = (line.end_ms - line.start_ms) / 5;
    let (from, to) = (line.start_ms + margin, line.end_ms - margin);
    let mut covered: BTreeMap<usize, u64> = BTreeMap::new();
    for turn in &result.turns {
        let overlap = turn.end_ms.min(to).saturating_sub(turn.start_ms.max(from));
        *covered.entry(turn.speaker).or_default() += overlap;
    }
    covered.into_iter().max_by_key(|(_, ms)| *ms).filter(|(_, ms)| *ms * 2 > to - from).map(|(s, _)| s)
}

#[test]
#[ignore = "needs TF_DIARIZE_FIXTURE_DIR and the installed diarization model"]
fn every_scripted_line_is_attributed_to_its_voice() {
    let dir = PathBuf::from(std::env::var("TF_DIARIZE_FIXTURE_DIR").expect("set TF_DIARIZE_FIXTURE_DIR"));
    let lines = load_lines(&dir);
    let result = diarize(&dir);
    let voices = lines.iter().map(|line| line.voice.clone()).collect::<BTreeSet<_>>();
    let speakers = result.turns.iter().map(|turn| turn.speaker).collect::<BTreeSet<_>>();
    println!("{} turns, speakers {speakers:?}", result.turns.len());
    for turn in &result.turns {
        println!(
            "  {:>6}-{:>6} speaker {} overlap {}",
            turn.start_ms, turn.end_ms, turn.speaker, turn.overlap
        );
    }
    assert_eq!(speakers.len(), voices.len(), "expected one speaker per voice");

    let mut voice_to_speaker: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
    for line in lines.iter().filter(|line| !line.overlaps_previous) {
        let speaker = dominant_speaker(&result, line).unwrap_or_else(|| {
            panic!("no dominant speaker for the {} line at {} ms", line.voice, line.start_ms)
        });
        voice_to_speaker.entry(line.voice.clone()).or_default().insert(speaker);
    }
    for (voice, mapped) in &voice_to_speaker {
        assert_eq!(mapped.len(), 1, "{voice} was split across speakers {mapped:?}");
    }
    let distinct = voice_to_speaker.values().flatten().collect::<BTreeSet<_>>();
    assert_eq!(distinct.len(), voices.len(), "two voices were merged into one speaker");
}

#[test]
#[ignore = "needs TF_DIARIZE_FIXTURE_DIR and the installed diarization model"]
fn turns_carry_their_own_embeddings() {
    let dir = PathBuf::from(std::env::var("TF_DIARIZE_FIXTURE_DIR").expect("set TF_DIARIZE_FIXTURE_DIR"));
    let result = diarize(&dir);
    let long = result.turns.iter().filter(|turn| turn.end_ms - turn.start_ms >= 1_000);
    for turn in long {
        let embedding = turn.embedding.as_ref().expect("turns of a second or more have an embedding");
        assert_eq!(embedding.len(), 256);
    }
}
