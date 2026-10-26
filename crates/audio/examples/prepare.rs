//! `cargo run -p tf-audio --example prepare -- in.wav out.wav [low|normal|high]`

use tf_audio::resample;
use tf_audio::vad::{self, Sensitivity};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(input), Some(output)) = (args.next(), args.next()) else {
        eprintln!("usage: prepare <in.wav> <out.wav> [low|normal|high]");
        std::process::exit(2);
    };
    let sensitivity = match args.next().as_deref() {
        Some("low") => Sensitivity::Low,
        Some("high") => Sensitivity::High,
        _ => Sensitivity::Normal,
    };

    let (samples, rate, channels) = read_wav(&input)?;
    eprintln!("input: {rate} Hz, {channels} ch, {:.2}s", samples.len() as f64 / rate as f64);

    let mono: Vec<f32> = if channels == 1 {
        samples
    } else {
        samples
            .chunks_exact(channels as usize)
            .map(|frame| frame.iter().sum::<f32>() / f32::from(channels))
            .collect()
    };
    let at_16k = resample::to_16k(&mono, rate);
    let trimmed = vad::trim(&at_16k, 16_000, sensitivity);
    eprintln!(
        "trimmed: {:.2}s ({} ms speech, {} samples)",
        trimmed.samples.len() as f64 / 16_000.0,
        trimmed.speech_ms,
        trimmed.samples.len()
    );

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&output, spec)?;
    for sample in &trimmed.samples {
        writer.write_sample((sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16)?;
    }
    writer.finalize()?;
    println!("wrote {output}");
    Ok(())
}

fn read_wav(path: &str) -> Result<(Vec<f32>, u32, u16), Box<dyn std::error::Error>> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let samples = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<Vec<_>, _>>()?,
        hound::SampleFormat::Int => {
            let scale = (1u64 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().map(|s| s.map(|s| s as f32 / scale)).collect::<Result<Vec<_>, _>>()?
        }
    };
    Ok((samples, spec.sample_rate, spec.channels))
}
