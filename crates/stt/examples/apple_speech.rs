//! `cargo run -p tf-stt --example apple_speech -- <16kHz-mono.wav> [locale]`

use std::path::PathBuf;

use tf_stt::{EngineSpec, SAMPLE_RATE, TranscribeOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let Some(wav) = args.next() else {
        eprintln!("usage: apple_speech <16kHz-mono.wav> [locale]");
        std::process::exit(2);
    };
    let locale = args.next().unwrap_or_else(|| "en-US".to_owned());
    let vocabulary: Vec<String> = args.collect();

    let locales = tf_apple::speech::locales()?;
    let installed: Vec<_> = locales.iter().filter(|locale| locale.installed).collect();
    eprintln!("{} supported locales, {} installed", locales.len(), installed.len());
    for locale in installed.iter().take(5) {
        eprintln!("  {} ({})", locale.id, locale.name);
    }

    let samples = read_wav(&PathBuf::from(wav))?;
    eprintln!("{:.1}s of audio, locale {locale}", samples.len() as f32 / SAMPLE_RATE as f32);
    let engine = EngineSpec::AppleSpeech { locale };
    let mut transcriber = tf_stt::load(&engine)?;
    let started = std::time::Instant::now();
    let options = TranscribeOptions { language: None, vocabulary: &vocabulary };
    let text = transcriber.transcribe(&samples, &options)?;
    eprintln!("transcribed in {} ms", started.elapsed().as_millis());
    println!("{text}");
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
