use evdev::Key;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keystroke {
    pub key: Key,
    pub shift: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorrectionPlan {
    pub buffer: Vec<Keystroke>,
    pub extra_backspaces: usize,
}

const EXCLUDED_WORDS: &[&str] = &[
    "sudo", "git", "cargo", "rustc", "python", "node", "grep", "echo", "ls", "cd", "rm", "mkdir",
    "apt",
];

pub fn manual_correction_plan(
    current_buffer: &[Keystroke],
    last_word_buffer: &[Keystroke],
    last_word_followed_by_separator: bool,
) -> Option<CorrectionPlan> {
    if !current_buffer.is_empty() {
        return Some(CorrectionPlan {
            buffer: current_buffer.to_vec(),
            extra_backspaces: 0,
        });
    }

    if !last_word_buffer.is_empty() {
        return Some(CorrectionPlan {
            buffer: last_word_buffer.to_vec(),
            extra_backspaces: usize::from(last_word_followed_by_separator),
        });
    }

    None
}

pub fn should_switch(buffer: &[Keystroke]) -> bool {
    if buffer.len() < 3 {
        return false;
    }

    let word = keys_to_string(buffer);
    if is_likely_english(&word) {
        return false;
    }

    let mut score = 0;

    for (index, key) in buffer.iter().enumerate() {
        if matches!(
            key.key,
            Key::KEY_LEFTBRACE
                | Key::KEY_RIGHTBRACE
                | Key::KEY_SEMICOLON
                | Key::KEY_APOSTROPHE
                | Key::KEY_GRAVE
        ) {
            score += 15;
        }
        if matches!(key.key, Key::KEY_COMMA | Key::KEY_DOT) {
            score += if index < buffer.len() - 1 { 15 } else { 5 };
        }
    }

    let rus_vowels = count_russian_vowels(buffer);
    let eng_vowels = word.chars().filter(|c| "aeiouy".contains(*c)).count();

    if rus_vowels > eng_vowels {
        score += 10;
    }
    if eng_vowels == 0 {
        score += 12;
    }

    score >= 10
}

fn keys_to_string(keys: &[Keystroke]) -> String {
    keys.iter()
        .filter_map(|stroke| {
            let name = format!("{:?}", stroke.key);
            name.strip_prefix("KEY_").map(|value| value.to_lowercase())
        })
        .collect()
}

fn count_russian_vowels(keys: &[Keystroke]) -> usize {
    keys.iter()
        .filter(|stroke| {
            matches!(
                stroke.key,
                Key::KEY_F
                    | Key::KEY_T
                    | Key::KEY_GRAVE
                    | Key::KEY_B
                    | Key::KEY_J
                    | Key::KEY_E
                    | Key::KEY_S
                    | Key::KEY_APOSTROPHE
                    | Key::KEY_DOT
                    | Key::KEY_Z
            )
        })
        .count()
}

fn is_likely_english(word: &str) -> bool {
    let clean_word = word.trim_end_matches(&['.', ',', ';', '\'', '`', '[', ']'][..]);
    if clean_word.len() < 3 || EXCLUDED_WORDS.contains(&clean_word) {
        return true;
    }

    let eng_vowels = clean_word.chars().filter(|c| "aeiouy".contains(*c)).count();
    eng_vowels >= 2 && clean_word.len() <= 5
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(key: Key) -> Keystroke {
        Keystroke { key, shift: false }
    }

    #[test]
    fn manual_plan_prefers_current_buffer() {
        let current = vec![stroke(Key::KEY_F), stroke(Key::KEY_D), stroke(Key::KEY_L)];
        let last = vec![stroke(Key::KEY_A)];

        let plan = manual_correction_plan(&current, &last, true).unwrap();
        assert_eq!(plan.buffer, current);
        assert_eq!(plan.extra_backspaces, 0);
    }

    #[test]
    fn previous_word_without_separator_does_not_backspace_extra_char() {
        let current = vec![];
        let last = vec![stroke(Key::KEY_F), stroke(Key::KEY_D), stroke(Key::KEY_L)];

        let plan = manual_correction_plan(&current, &last, false).unwrap();
        assert_eq!(plan.buffer, last);
        assert_eq!(plan.extra_backspaces, 0);
    }

    #[test]
    fn previous_word_with_separator_backspaces_separator_too() {
        let current = vec![];
        let last = vec![stroke(Key::KEY_F), stroke(Key::KEY_D), stroke(Key::KEY_L)];

        let plan = manual_correction_plan(&current, &last, true).unwrap();
        assert_eq!(plan.buffer, last);
        assert_eq!(plan.extra_backspaces, 1);
    }

    #[test]
    fn russian_like_word_triggers_switch() {
        let buffer = vec![stroke(Key::KEY_F), stroke(Key::KEY_D), stroke(Key::KEY_L)];
        assert!(should_switch(&buffer));
    }

    #[test]
    fn common_english_word_does_not_trigger_switch() {
        let buffer = vec![stroke(Key::KEY_C), stroke(Key::KEY_A), stroke(Key::KEY_R)];
        assert!(!should_switch(&buffer));
    }
}
