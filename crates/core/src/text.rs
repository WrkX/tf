//! ersetzungen und fuzzy-korrektur für vokabular. reihenfolge: stt, dann replacements, dann fuzzy-vokabular, dann llm. der fuzzy-schritt greift lieber zu wenig. eine falsche korrektur ist ärgerlicher als eine verpasste.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Vocabulary {
    pub terms: Vec<Term>,
    pub replacements: Vec<Replacement>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Term {
    pub text: String,
    /// exakte begriffe werden nie fuzzy-korrigiert; nur replacements fassen sie an.
    pub exact: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Replacement {
    pub from: String,
    pub to: String,
    pub kind: ReplacementKind,
    pub case_sensitive: bool,
}

impl Default for Replacement {
    fn default() -> Self {
        Self { from: String::new(), to: String::new(), kind: ReplacementKind::Phrase, case_sensitive: false }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReplacementKind {
    #[default]
    Phrase,
    Regex,
}

impl Vocabulary {
    pub fn terms_text(&self) -> Vec<String> {
        self.terms.iter().map(|term| term.text.trim().to_owned()).filter(|text| !text.is_empty()).collect()
    }

    pub fn apply(&self, text: &str) -> String {
        let text = apply_replacements(text, &self.replacements);
        apply_vocabulary(&text, &self.terms)
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty() && self.replacements.is_empty()
    }
}

pub fn apply_replacements(text: &str, replacements: &[Replacement]) -> String {
    let mut text = text.to_owned();
    for replacement in replacements {
        let from = replacement.from.trim();
        if from.is_empty() {
            continue;
        }
        text = match replacement.kind {
            ReplacementKind::Phrase => {
                replace_phrase(&text, from, &replacement.to, replacement.case_sensitive)
            }
            ReplacementKind::Regex => replace_regex(&text, from, &replacement.to, replacement.case_sensitive),
        };
    }
    text
}

fn replace_phrase(text: &str, needle: &str, replacement: &str, case_sensitive: bool) -> String {
    let haystack: Vec<char> = text.chars().collect();
    let needle_chars: Vec<char> = needle.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < haystack.len() {
        let end = index + needle_chars.len();
        let matches = end <= haystack.len()
            && haystack[index..end]
                .iter()
                .zip(&needle_chars)
                .all(|(a, b)| if case_sensitive { a == b } else { a.to_lowercase().eq(b.to_lowercase()) })
            && (index == 0 || !haystack[index - 1].is_alphanumeric())
            && (end == haystack.len() || !haystack[end].is_alphanumeric());
        if matches {
            out.push_str(replacement);
            index = end;
        } else {
            out.push(haystack[index]);
            index += 1;
        }
    }
    out
}

fn replace_regex(text: &str, pattern: &str, replacement: &str, case_sensitive: bool) -> String {
    let regex = regex_lite::RegexBuilder::new(pattern).case_insensitive(!case_sensitive).build();
    match regex {
        Ok(regex) => regex.replace_all(text, replacement).into_owned(),
        Err(_) => text.to_owned(),
    }
}

/// prüft ein replacement beim tippen. `Ok` für phrasen, ein kompilier-fehler für kaputte regexe.
pub fn validate_replacement(replacement: &Replacement) -> Result<(), String> {
    if replacement.from.trim().is_empty() {
        return Err("From is empty".into());
    }
    if replacement.kind == ReplacementKind::Regex {
        regex_lite::RegexBuilder::new(&replacement.from)
            .case_insensitive(!replacement.case_sensitive)
            .build()
            .map(|_| ())
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct Token {
    normalized: String,
    start: usize,
    end: usize,
}

fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut start = None;
    for (index, ch) in text.char_indices() {
        if ch.is_alphanumeric() {
            start.get_or_insert(index);
        } else if let Some(begin) = start.take() {
            tokens.push(Token { normalized: normalize(&text[begin..index]), start: begin, end: index });
        }
    }
    if let Some(begin) = start {
        tokens.push(Token { normalized: normalize(&text[begin..]), start: begin, end: text.len() });
    }
    tokens
}

pub fn apply_vocabulary(text: &str, terms: &[Term]) -> String {
    let terms: Vec<(&Term, String)> = terms
        .iter()
        .filter(|term| !term.exact && term.text.trim().chars().count() >= 4)
        .map(|term| (term, normalize(&term.text)))
        .filter(|(_, normalized)| !normalized.is_empty())
        .collect();
    if terms.is_empty() {
        return text.to_owned();
    }

    let tokens = tokenize(text);
    if tokens.is_empty() {
        return text.to_owned();
    }
    let exact_terms: Vec<(String, String)> = terms
        .iter()
        .map(|(term, normalized)| (normalized.replace(' ', ""), term.text.trim().to_owned()))
        .collect();

    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let mut best: Option<(usize, usize, f64, &Term)> = None;
        for (term, normalized) in &terms {
            let words = normalized.split(' ').count().max(1);
            for length in [words, words.saturating_sub(1), words + 1] {
                if length == 0 || index + length > tokens.len() {
                    continue;
                }
                let end = index + length;
                let joined: String =
                    tokens[index..end].iter().map(|token| token.normalized.as_str()).collect();
                if joined.is_empty() || joined == *normalized {
                    continue;
                }
                // fenster nie umschreiben, das schon ein anderer bekannter begriff ist.
                if exact_terms.iter().any(|(known, canonical)| {
                    *known == joined && !canonical.eq_ignore_ascii_case(term.text.trim())
                }) {
                    continue;
                }
                let score = similarity(&joined, normalized);
                let accepted = if term_chars(term) >= 4 { score >= 0.82 } else { score >= 0.9 }
                    || (phonetic_key(&joined) == phonetic_key(normalized) && score >= 0.65);
                if !accepted {
                    continue;
                }
                let better = best.map(|(_, _, best_score, _)| score > best_score).unwrap_or(true);
                if better {
                    best = Some((length, end, score, term));
                }
            }
        }
        match best {
            Some((_, end, _, term)) => {
                edits.push((tokens[index].start, tokens[end - 1].end, term.text.trim().to_owned()));
                index = end;
            }
            None => index += 1,
        }
    }

    if edits.is_empty() {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for (start, end, replacement) in edits {
        out.push_str(&text[cursor..start]);
        out.push_str(&replacement);
        cursor = end;
    }
    out.push_str(&text[cursor..]);
    out
}

fn term_chars(term: &Term) -> usize {
    normalize(&term.text).chars().count()
}

fn similarity(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let max = a.len().max(b.len());
    if max == 0 {
        return 1.0;
    }
    1.0 - levenshtein(&a, &b) as f64 / max as f64
}

fn levenshtein(a: &[char], b: &[char]) -> usize {
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let substitution = previous[j] + usize::from(ca != cb);
            current[j + 1] = substitution.min(previous[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

/// kompakter soundex-artiger schlüssel, reicht für kleine transkriptionsfehler.
fn phonetic_key(word: &str) -> String {
    let letters: Vec<char> = word.chars().filter(|ch| ch.is_alphabetic()).collect();
    let Some(first) = letters.first() else {
        return String::new();
    };
    let mut key = String::new();
    key.push(*first);
    let mut last = digit_for(*first);
    for ch in letters.iter().skip(1) {
        let digit = digit_for(*ch);
        if let Some(digit) = digit {
            if Some(digit) != last {
                key.push(digit);
            }
            last = Some(digit);
        } else if !matches!(ch, 'a' | 'e' | 'i' | 'o' | 'u' | 'y' | 'h' | 'w') {
            last = None;
        }
    }
    key
}

fn digit_for(ch: char) -> Option<char> {
    Some(match ch {
        'b' | 'f' | 'p' | 'v' => '1',
        'c' | 'g' | 'j' | 'k' | 'q' | 's' | 'x' | 'z' => '2',
        'd' | 't' => '3',
        'l' => '4',
        'm' | 'n' => '5',
        'r' => '6',
        _ => return None,
    })
}

/// macht klein, entfernt diakritika und alles, was kein buchstabe oder keine ziffer ist. wörter eines mehrwort-begriffs werden mit einem leerzeichen verbunden.
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for ch in text.chars() {
        for mapped in fold(ch) {
            if mapped.is_alphanumeric() {
                if pending_space && !out.is_empty() {
                    out.push(' ');
                }
                pending_space = false;
                out.extend(mapped.to_lowercase());
            } else {
                pending_space = true;
            }
        }
    }
    out
}

fn fold(ch: char) -> impl Iterator<Item = char> {
    let folded: &str = match ch {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'ç' | 'ć' | 'č' => "c",
        'ď' | 'đ' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' => "e",
        'ğ' => "g",
        'ì' | 'í' | 'î' | 'ï' | 'ī' | 'į' => "i",
        'ł' => "l",
        'ñ' | 'ń' | 'ň' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' => "o",
        'ř' => "r",
        'ś' | 'š' | 'ş' => "s",
        'ß' => "ss",
        'ť' => "t",
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' => "u",
        'ý' | 'ÿ' => "y",
        'ž' | 'ź' | 'ż' => "z",
        'æ' => "ae",
        'œ' => "oe",
        _ => {
            return std::iter::once(ch).collect::<Vec<_>>().into_iter();
        }
    };
    folded.chars().collect::<Vec<_>>().into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(text: &str) -> Term {
        Term { text: text.into(), exact: false }
    }

    #[test]
    fn phrase_replacements_are_whole_words_and_case_insensitive() {
        let replacements =
            vec![Replacement { from: "new paragraph".into(), to: "\n\n".into(), ..Replacement::default() }];
        assert_eq!(apply_replacements("One. New Paragraph Two.", &replacements), "One. \n\n Two.");
        assert_eq!(apply_replacements("newparagraphed", &replacements), "newparagraphed");
    }

    #[test]
    fn regex_replacements_work_and_invalid_patterns_are_ignored() {
        let replacements = vec![Replacement {
            from: r"\bteh\b".into(),
            to: "the".into(),
            kind: ReplacementKind::Regex,
            case_sensitive: false,
        }];
        assert_eq!(apply_replacements("teh cat", &replacements), "the cat");
        let invalid = vec![Replacement {
            from: "(".into(),
            to: "x".into(),
            kind: ReplacementKind::Regex,
            ..Replacement::default()
        }];
        assert_eq!(apply_replacements("a(b", &invalid), "a(b");
    }

    #[test]
    fn fuzzy_corrects_a_one_letter_error_and_keeps_punctuation() {
        assert_eq!(
            apply_vocabulary("I deployed kubernets, then left.", &[term("Kubernetes")]),
            "I deployed Kubernetes, then left."
        );
    }

    #[test]
    fn fuzzy_does_not_turn_unrelated_words_into_terms() {
        assert_eq!(apply_vocabulary("pass me the cube", &[term("Kubernetes")]), "pass me the cube");
    }

    #[test]
    fn exact_terms_are_never_fuzzy_corrected() {
        let exact = Term { text: "Kubernetes".into(), exact: true };
        assert_eq!(apply_vocabulary("kubernets", &[exact]), "kubernets");
    }

    #[test]
    fn multi_word_terms_handle_split_and_merged_words() {
        assert_eq!(apply_vocabulary("we use newyork daily", &[term("New York")]), "we use New York daily");
        assert_eq!(apply_vocabulary("we use new york daily", &[term("New York")]), "we use New York daily");
    }

    #[test]
    fn diacritics_are_folded_when_comparing() {
        assert_eq!(apply_vocabulary("cafe creme", &[term("Café Crème")]), "Café Crème");
    }

    #[test]
    fn terms_shorter_than_four_characters_need_a_near_exact_match() {
        assert_eq!(apply_vocabulary("we use tf", &[term("tf")]), "we use tf");
        assert_eq!(apply_vocabulary("we use TV", &[term("tf")]), "we use TV");
    }

    #[test]
    fn replacements_run_before_vocabulary() {
        let vocabulary = Vocabulary {
            terms: vec![term("Kubernetes")],
            replacements: vec![Replacement {
                from: "cube".into(),
                to: "kubernets".into(),
                ..Replacement::default()
            }],
        };
        assert_eq!(vocabulary.apply("the cube cluster"), "the Kubernetes cluster");
    }

    #[test]
    fn replacement_validation_reports_bad_regex() {
        let good = Replacement {
            from: r"\bteh\b".into(),
            to: "the".into(),
            kind: ReplacementKind::Regex,
            case_sensitive: false,
        };
        assert!(validate_replacement(&good).is_ok());
        let bad = Replacement { from: "(".into(), ..good.clone() };
        assert!(validate_replacement(&bad).is_err());
        let empty = Replacement { from: "  ".into(), ..good };
        assert!(validate_replacement(&empty).is_err());
    }

    #[test]
    fn phonetic_key_pairs_common_misspellings() {
        assert_eq!(phonetic_key("kubernets"), phonetic_key("kubernetes"));
        assert_ne!(phonetic_key("cube"), phonetic_key("kubernetes"));
    }
}
