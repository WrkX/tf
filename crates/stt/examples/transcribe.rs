use std::path::PathBuf;
use std::time::Instant;

use tf_stt::{Parakeet, SAMPLE_RATE, TranscribeOptions, Transcriber};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(models), Some(wav)) = (args.next(), args.next()) else {
        eprintln!("usage: transcribe <model-dir> <16kHz-mono.wav>");
        std::process::exit(2);
    };

    let samples = read_wav(&PathBuf::from(wav))?;
    let seconds = samples.len() as f32 / SAMPLE_RATE as f32;

    let t = Instant::now();
    let mut engine = Parakeet::load(&PathBuf::from(models))?;
    eprintln!("load      {:>7.0} ms", t.elapsed().as_secs_f32() * 1e3);

    for run in ["cold", "warm"] {
        let t = Instant::now();
        let text = engine.transcribe(&samples, &TranscribeOptions::default())?;
        let ms = t.elapsed().as_secs_f32() * 1e3;
        eprintln!("{run:<9} {ms:>7.0} ms  ({:.0}x realtime, {seconds:.1}s audio)", seconds * 1e3 / ms);
        if run == "warm" {
            println!("{text}");
        }
    }
    if std::env::var_os("TF_HOLD").is_some() {
        eprintln!("holding; read RSS with ps");
        std::thread::sleep(std::time::Duration::from_secs(30));
    }
    Ok(())
}

fn read_wav(path: &PathBuf) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    if spec.sample_rate != SAMPLE_RATE || spec.channels != 1 {
        return Err(format!("expected 16 kHz mono, got {} Hz x{}", spec.sample_rate, spec.channels).into());
    }
    Ok(match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = (1u32 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().map(|s| s.map(|s| s as f32 / scale)).collect::<Result<_, _>>()?
        }
    })
}
