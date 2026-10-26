//! baut eine geskriptete unterhaltung mit mehreren stimmen über macos `say` für den diarisierungs-fixture-test. nichts davon wird eingecheckt.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Result, cmd};

const RATE: usize = 16_000;

/// `(voice, text, pause in ms vor der zeile; negativ überlappt die vorherige zeile)`.
const SCRIPT: &[(&str, &str, i64)] = &[
    (
        "Samantha",
        "Hi everyone, thanks for joining. Today we will go over the roadmap and then the budget for next quarter.",
        300,
    ),
    (
        "Daniel",
        "Sounds good. I have prepared the numbers for the first two projects and I can share them right away.",
        500,
    ),
    ("Fred", "Before that, can we confirm who owns the migration work? I think it slipped last time.", 600),
    (
        "Samantha",
        "Good point. I believe the platform team owns it, but let me double check with them after this call.",
        400,
    ),
    (
        "Daniel",
        "The platform team told me yesterday that they would need two more engineers to finish it on time.",
        500,
    ),
    (
        "Fred",
        "Then we should flag that as a risk in the plan, and decide whether to move the launch date.",
        -700,
    ),
    (
        "Samantha",
        "Agreed. Let us add it to the risks section, and Daniel, please send the numbers by the end of the day.",
        500,
    ),
    ("Daniel", "Will do. I will also include the hiring estimate so that everyone can see the cost.", 400),
];

pub fn run(dir: Option<String>) -> Result {
    let out = PathBuf::from(dir.unwrap_or_else(|| "/tmp/tf-diarize-fixture".into()));
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let mut timeline: Vec<f32> = Vec::new();
    let mut lines = Vec::new();
    let mut end_ms: i64 = 0;
    for (index, (voice, text, gap)) in SCRIPT.iter().enumerate() {
        let wav = speak(&out, index, voice, text)?;
        let samples = read_wav(&wav)?;
        let start_ms = (end_ms + gap).max(0);
        let start = start_ms as usize * RATE / 1_000;
        if timeline.len() < start + samples.len() {
            timeline.resize(start + samples.len(), 0.0);
        }
        for (offset, sample) in samples.iter().enumerate() {
            timeline[start + offset] += sample;
        }
        let length_ms = samples.len() * 1_000 / RATE;
        end_ms = (start_ms as usize + length_ms) as i64;
        lines.push(serde_json::json!({
            "voice": voice,
            "text": text,
            "start_ms": start_ms,
            "end_ms": end_ms,
            "overlaps_previous": *gap < 0,
        }));
    }
    let mix = out.join("mix.wav");
    write_wav(&mix, &timeline)?;
    // dieselben codec-einstellungen wie beim schreiben der "Them"-spur.
    let m4a = out.join("mix.m4a");
    cmd(Command::new("afconvert").args(["-f", "m4af", "-d", "aac", "-b", "32000"]).arg(&mix).arg(&m4a))?;
    let manifest = serde_json::json!({ "lines": lines });
    fs::write(out.join("fixture.json"), serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    println!("fixture written to {}", out.display());
    println!(
        "run: TF_DIARIZE_FIXTURE_DIR={} cargo test -p tf-diarize --test fixture -- --ignored --nocapture",
        out.display()
    );
    Ok(())
}

fn speak(dir: &Path, index: usize, voice: &str, text: &str) -> Result<PathBuf> {
    let aiff = dir.join(format!("line{index}.aiff"));
    let wav = dir.join(format!("line{index}.wav"));
    cmd(Command::new("say").args(["-v", voice, "-o"]).arg(&aiff).arg(text))?;
    cmd(Command::new("afconvert").args(["-f", "WAVE", "-d", "LEI16@16000", "-c", "1"]).arg(&aiff).arg(&wav))?;
    Ok(wav)
}

/// liest 16-bit-mono-pcm aus einer riff-datei und überspringt unbekannte chunks.
fn read_wav(path: &Path) -> Result<Vec<f32>> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        if &bytes[at..at + 4] == b"data" {
            let data = &bytes[at + 8..(at + 8 + size).min(bytes.len())];
            return Ok(data
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| i16::from_le_bytes(*c) as f32 / 32768.0)
                .collect());
        }
        at += 8 + size + (size & 1);
    }
    Err(format!("{}: no data chunk", path.display()))
}

fn write_wav(path: &Path, samples: &[f32]) -> Result {
    let data = samples
        .iter()
        .flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())
        .collect::<Vec<u8>>();
    let mut out = Vec::with_capacity(44 + data.len());
    out.extend(b"RIFF");
    out.extend((36 + data.len() as u32).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend((RATE as u32).to_le_bytes());
    out.extend((RATE as u32 * 2).to_le_bytes());
    out.extend(2u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(data);
    fs::write(path, out).map_err(|e| e.to_string())
}
