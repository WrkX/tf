//! hält den llm-schritt in der sprache des transkripts.

const ENGLISH: &str = "en";

/// englischer name der sprache, in der `text` gesprochen wurde.
pub fn name(text: &str) -> Option<String> {
    tf_apple::language::detect(text, &[]).and_then(|code| tf_apple::language::name(&code))
}

/// true, wenn `output` nicht mehr in der sprache von `input` ist. kleine modelle übersetzen wegen des englischen prompts ins englische, also wird nur zwischen quellsprache und englisch entschieden; das bleibt auch bei sehr kurzen sätzen stabil.
pub fn drifted(input: &str, output: &str) -> bool {
    let Some(source) = tf_apple::language::detect(input, &[]) else {
        return false;
    };
    if source == ENGLISH {
        return false;
    }
    let candidates = [source.as_str(), ENGLISH];
    let expected = tf_apple::language::detect(input, &candidates);
    expected.is_some() && tf_apple::language::detect(output, &candidates) != expected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translations_into_english_are_caught() {
        assert!(drifted("Okay, dann machen wir das einfach so.", "Okay, then make it simple."));
        assert!(drifted("Und was macht Gap genau?", "Gap exactly."));
    }

    #[test]
    fn same_language_with_english_terms_is_kept() {
        assert!(!drifted(
            "Wir haben einen Button, der heißt Show Live Transcript.",
            "Wir haben einen Button, der heißt Show Live Transcript."
        ));
        assert!(!drifted("Sure, no problem at all.", "Sure, no problem at all."));
    }

    #[test]
    fn language_names_are_english() {
        assert_eq!(name("Ich brauche ein Icon für meine App.").as_deref(), Some("German"));
    }
}
