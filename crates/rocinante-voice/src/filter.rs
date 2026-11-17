//! Clean whisper's output and drop what it invents. Whisper was trained on
//! subtitles, so on noise it can "hear" stock captions ("Thanks for
//! watching!") or annotations ("[BLANK_AUDIO]", "(keyboard clicking)").
//! Real short answers ("yes", "okay, do it") must pass untouched.

/// Phrases whisper produces from non-speech, compared after normalizing
/// (lowercase, punctuation stripped).
const HALLUCINATIONS: &[&str] = &[
    "you",
    "thank you for watching",
    "thanks for watching",
    "thank you so much for watching",
    "please subscribe",
    "subtitles by the amaraorg community",
    "transcription by castingwords",
];

/// The transcript to send, or `None` when nothing real was said.
pub fn clean(raw: &str) -> Option<String> {
    let text = strip_annotations(raw);
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if !text.chars().any(char::is_alphanumeric) {
        return None;
    }
    let norm: String = text
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect();
    let norm = norm.split_whitespace().collect::<Vec<_>>().join(" ");
    if HALLUCINATIONS.contains(&norm.as_str()) {
        return None;
    }
    Some(text)
}

/// Remove `[...]`, `(...)` and `*...*` spans: whisper's sound annotations.
fn strip_annotations(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut closing: Option<char> = None;
    for c in s.chars() {
        match closing {
            Some(end) if c == end => closing = None,
            Some(_) => {}
            None => match c {
                '[' => closing = Some(']'),
                '(' => closing = Some(')'),
                '*' => closing = Some('*'),
                _ => out.push(c),
            },
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_real_speech() {
        assert_eq!(
            clean(" Can you add a test for the parser? "),
            Some("Can you add a test for the parser?".into())
        );
        assert_eq!(clean("Yes."), Some("Yes.".into()));
        assert_eq!(clean("Okay, do it."), Some("Okay, do it.".into()));
        assert_eq!(clean("Thank you."), Some("Thank you.".into()));
    }

    #[test]
    fn drops_annotations_and_stock_captions() {
        assert_eq!(clean("[BLANK_AUDIO]"), None);
        assert_eq!(clean("(keyboard clicking)"), None);
        assert_eq!(clean(" *coughs* "), None);
        assert_eq!(clean("you"), None);
        assert_eq!(clean("You."), None);
        assert_eq!(clean("Thanks for watching!"), None);
        assert_eq!(clean("..."), None);
        assert_eq!(clean(""), None);
    }

    #[test]
    fn strips_annotations_inside_speech() {
        assert_eq!(
            clean("[MUSIC] Run the tests (music) please"),
            Some("Run the tests please".into())
        );
    }
}
