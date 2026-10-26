//! `cargo run -p tf-models --example download -- <model-id> [cancel-after-mb]`

use std::sync::mpsc;
use std::time::Duration;

use tf_models::{Catalog, Progress, install};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let Some(id) = args.next() else {
        eprintln!("usage: download <model-id> [cancel-after-mb]");
        std::process::exit(2);
    };
    let cancel_after = args.next().and_then(|value| value.parse::<u64>().ok());

    let catalog = Catalog::embedded()?;
    let entry = catalog.find(&id).ok_or_else(|| format!("unknown model {id}"))?;
    let root = support_dir();
    let (tx, rx) = mpsc::channel();
    let download = install(entry, &root, move |progress| {
        let _ = tx.send(progress);
    });

    let mut cancelled = false;
    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Progress::Started { total }) => eprintln!("started: {:.0} MB", total as f64 / 1e6),
            Ok(Progress::Advanced { done }) => {
                eprintln!("{:.0} MB", done as f64 / 1e6);
                if let Some(limit) = cancel_after
                    && !cancelled
                    && done > limit * 1_000_000
                {
                    eprintln!("cancelling at {limit} MB");
                    download.cancel();
                    cancelled = true;
                }
            }
            Ok(Progress::Verifying) => eprintln!("verifying…"),
            Ok(Progress::Installed) => {
                println!("installed {id}");
                break;
            }
            Ok(Progress::Failed(e)) => return Err(e.into()),
            Ok(Progress::Cancelled) => {
                println!("cancelled");
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    download.join();
    Ok(())
}

fn support_dir() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("Library/Application Support/tf")
}
