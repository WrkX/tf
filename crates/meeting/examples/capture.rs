//! manuelles testprogramm: nimmt beide meeting-spuren ein paar sekunden auf und schreibt sie ins aktuelle verzeichnis: `cargo run -p tf-meeting --example capture -- 10`.

use std::time::{Duration, Instant};

use tf_meeting::{Capture, CaptureOptions};

fn main() {
    let secs = std::env::args().nth(1).and_then(|value| value.parse::<u64>().ok()).unwrap_or(10);
    let options = CaptureOptions { mic_uid: None, app_pids: Vec::new(), exclude_own_process: true };
    let capture = match Capture::start(&options) {
        Ok(capture) => capture,
        Err(error) => {
            eprintln!("capture failed: {error}");
            std::process::exit(1);
        }
    };
    println!(
        "capturing for {secs}s (mic {} Hz, system {} Hz, echo likely: {})",
        capture.me_rate(),
        capture.them_rate(),
        capture.echo_likely()
    );

    let started = Instant::now();
    let mut me = Vec::new();
    let mut them = Vec::new();
    while started.elapsed() < Duration::from_secs(secs) {
        std::thread::sleep(Duration::from_millis(100));
        me.extend(capture.drain_me());
        them.extend(capture.drain_them());
    }
    drop(capture);

    println!("mic: {} samples, system: {} samples", me.len(), them.len());
    if let Err(error) = tf_audio::file::write_m4a(std::path::Path::new("capture-me.m4a"), &me) {
        eprintln!("could not write capture-me.m4a: {error}");
    }
    if let Err(error) = tf_audio::file::write_m4a(std::path::Path::new("capture-them.m4a"), &them) {
        eprintln!("could not write capture-them.m4a: {error}");
    }
}
