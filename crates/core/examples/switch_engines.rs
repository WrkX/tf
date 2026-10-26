//! wechselt engines ohne neustart und prüft die event-reihenfolge. `cargo run -p tf-core --example switch_engines`

use std::sync::mpsc;
use std::time::{Duration, Instant};

use tf_core::{Config, Core, Event};

fn main() {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
    let models = home.join("Library/Application Support/tf/models");
    let mut config = Config::default();
    config.engine.default = "parakeet-tdt-0.6b-v3".to_owned();

    let (tx, rx) = mpsc::channel();
    let core = Core::spawn(config.clone(), models, move |event| {
        let _ = tx.send(event);
    });

    for engine in ["parakeet-tdt-0.6b-v3", "parakeet-tdt-0.6b-v2", "apple-speech:en-US"] {
        if engine != config.engine.default {
            config.engine.default = engine.to_owned();
            core.reload(config.clone());
        }
        let started = Instant::now();
        let mut loading = false;
        loop {
            match rx.recv_timeout(Duration::from_secs(60)) {
                Ok(Event::EngineLoading) => loading = true,
                Ok(Event::EngineReady) => {
                    println!(
                        "{engine}: ready in {} ms (loading event: {loading})",
                        started.elapsed().as_millis()
                    );
                    break;
                }
                Ok(Event::EngineFailed(failure)) => {
                    eprintln!("{engine}: failed: {}", failure.message());
                    return;
                }
                Ok(_) => {}
                Err(e) => {
                    eprintln!("{engine}: no event: {e}");
                    return;
                }
            }
        }
    }
    core.cancel();
}
