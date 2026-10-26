
#[derive(Clone, Copy, Debug)]
pub struct Line<'a> {
    /// anzeige-label: "Me", "Them", "Speaker 2" oder ein vom nutzer vergebener name.
    pub speaker: &'a str,
    pub start_ms: u64,
    pub text: &'a str,
}

pub fn transcript<'a>(lines: impl IntoIterator<Item = Line<'a>>) -> String {
    lines
        .into_iter()
        .map(|line| format!("[{}] {}: {}", clock(line.start_ms), line.speaker, line.text.trim()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// markdown-export für copy und export: `**Anna** (00:09): …`.
pub fn transcript_markdown<'a>(lines: impl IntoIterator<Item = Line<'a>>) -> String {
    lines
        .into_iter()
        .map(|line| format!("**{}** ({}): {}", line.speaker, clock(line.start_ms), line.text.trim()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// teilt an segmentgrenzen. jeder chunk bleibt unter `max_chars`, es sei denn ein segment allein ist länger; das ist besser, als eine zeile zu zerschneiden.
pub fn chunks<'a>(lines: &[Line<'a>], max_chars: usize) -> Vec<String> {
    let max_chars = max_chars.max(400);
    let mut chunks = Vec::new();
    let mut current = String::new();
    for line in lines {
        let formatted = format!("[{}] {}: {}\n", clock(line.start_ms), line.speaker, line.text.trim());
        if !current.is_empty() && current.len() + formatted.len() > max_chars {
            chunks.push(std::mem::take(&mut current));
        }
        current.push_str(&formatted);
    }
    if !current.trim().is_empty() {
        chunks.push(current);
    }
    chunks
}

pub fn map_system_prompt() -> String {
    "You are summarizing part of a meeting transcript. Extract only what matters:\n\
     key points, decisions, action items and open questions. Preserve owners, dates and\n\
     numbers exactly. Output compact Markdown bullets. Do not invent anything. Keep the\n\
     transcript's language."
        .to_owned()
}

pub fn final_system_prompt(mode_prompt: &str) -> String {
    let mode_prompt = mode_prompt.trim();
    if mode_prompt.is_empty() {
        "You are given a meeting transcript with labelled speakers. \"Me\" is the user. \
         Write Markdown with Summary, Decisions, Action items and \
         Open questions. Only use information from the transcript. Write in the language the speakers use, \
         not the language of these instructions. Never repeat a point across sections: \
         action items need an owner or a concrete task, otherwise leave them out. \
         Ignore one-word acknowledgements and likely transcription noise."
            .to_owned()
    } else {
        mode_prompt.to_owned()
    }
}

pub fn map_user_prompt(part: &str) -> String {
    format!("<transcript>\n{part}\n</transcript>")
}

pub fn final_user_prompt(transcript_or_notes: &str) -> String {
    format!("<transcript>\n{transcript_or_notes}\n</transcript>")
}

pub fn clock(ms: u64) -> String {
    let total = ms / 1000;
    format!("{:02}:{:02}", total / 60, total % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_speakers_and_timestamps() {
        let text = transcript([
            Line { speaker: "Them", start_ms: 2_000, text: "Hi everyone" },
            Line { speaker: "Me", start_ms: 65_000, text: "  Yes.  " },
        ]);
        assert_eq!(text, "[00:02] Them: Hi everyone\n[01:05] Me: Yes.");
        let markdown = transcript_markdown([Line { speaker: "Anna", start_ms: 9_000, text: "Great" }]);
        assert_eq!(markdown, "**Anna** (00:09): Great");
    }

    #[test]
    fn chunks_split_on_segment_boundaries() {
        let lines = (0..10)
            .map(|index| Line {
                speaker: if index % 2 == 0 { "Me" } else { "Them" },
                start_ms: index * 1_000,
                text: "a sentence that takes some space",
            })
            .collect::<Vec<_>>();
        let chunks = chunks(&lines, 400);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.starts_with("[00:"));
            assert!(!chunk.contains("\n\n"));
        }
        let rejoined = chunks.concat();
        assert_eq!(rejoined.matches("] Me:").count() + rejoined.matches("] Them:").count(), 10);
    }

    #[test]
    fn a_single_large_segment_stays_intact() {
        let long = "x".repeat(1_000);
        let chunks = chunks(&[Line { speaker: "Me", start_ms: 0, text: &long }], 400);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].contains(&long));
    }
}
