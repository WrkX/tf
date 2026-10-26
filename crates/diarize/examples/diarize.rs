//! `cargo run -p tf-diarize --example diarize -- them.m4a`

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use tf_diarize::{Diarizer, Options};

fn main() {
    let Some(path) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: diarize <them.m4a> [models-dir]");
        std::process::exit(2);
    };
    let models = std::env::args().nth(2).map(PathBuf::from).unwrap_or_else(tf_core_paths::models_dir);
    let samples = match tf_audio::file::read(&path) {
        Ok(samples) => samples,
        Err(error) => {
            eprintln!("could not read {}: {error}", path.display());
            std::process::exit(1);
        }
    };
    let mut diarizer = match Diarizer::load(&models) {
        Ok(diarizer) => diarizer,
        Err(error) => {
            eprintln!("could not load the models: {error}");
            std::process::exit(1);
        }
    };
    let cancel = AtomicBool::new(false);
    let result = diarizer.run(
        &samples,
        &Options::default(),
        |fraction| {
            eprintln!("{:3.0}%", fraction * 100.0);
        },
        &cancel,
    );
    match result {
        Ok(diarization) => {
            for turn in &diarization.turns {
                println!(
                    "{:>8.3} {:>8.3}  Speaker {}{}  {}",
                    turn.start_ms as f64 / 1_000.0,
                    turn.end_ms as f64 / 1_000.0,
                    turn.speaker + 1,
                    if turn.overlap { " (overlap)" } else { "" },
                    if turn.embedding.is_some() { "embedding" } else { "" },
                );
            }
            eprintln!("{} turns, {} speakers", diarization.turns.len(), diarization.centroids.len());
        }
        Err(error) => {
            eprintln!("diarization failed: {error}");
            std::process::exit(1);
        }
    }
}

/// bleibt lokal, damit das beispiel `tf-core` nicht in die crate zieht.
mod tf_core_paths {
    use std::path::PathBuf;

    pub fn models_dir() -> PathBuf {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Library/Application Support/tf/models")
    }
}
