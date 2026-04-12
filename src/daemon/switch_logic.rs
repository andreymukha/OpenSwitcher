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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LetterCase {
    Upper,
    Lower,
}

const EXCLUDED_WORDS: &[&str] = &[
    "sudo", "git", "cargo", "rustc", "python", "node", "grep", "echo", "ls", "cd", "rm", "mkdir",
    "apt",
];

pub fn manual_correction_plan(
    current_buffer: &[Keystroke],
    last_word_buffer: &[Keystroke],
    last_word_followed_by_separator: bool,
    fix_two_capitals: bool,
    fix_accidental_caps_lock: bool,
) -> Option<CorrectionPlan> {
    if !current_buffer.is_empty() {
        return Some(CorrectionPlan {
            buffer: apply_case_fixes_to_strokes(
                current_buffer,
                fix_two_capitals,
                fix_accidental_caps_lock,
            ),
            extra_backspaces: 0,
        });
    }

    if !last_word_buffer.is_empty() {
        return Some(CorrectionPlan {
            buffer: apply_case_fixes_to_strokes(
                last_word_buffer,
                fix_two_capitals,
                fix_accidental_caps_lock,
            ),
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
    let normalized_word = normalize_word_for_switch_heuristics(&word);
    if is_likely_english(&normalized_word) {
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
    let eng_vowels = normalized_word
        .chars()
        .filter(|c| "aeiouy".contains(*c))
        .count();

    if rus_vowels > eng_vowels {
        score += 10;
    }
    if eng_vowels == 0 {
        score += 12;
    }

    score >= 10
}

fn normalize_word_for_switch_heuristics(word: &str) -> String {
    word.chars().flat_map(|ch| ch.to_lowercase()).collect()
}

fn keys_to_string(keys: &[Keystroke]) -> String {
    let mut result = String::with_capacity(keys.len());
    for stroke in keys {
        if let Some(ch) = key_to_char(stroke.key, stroke.shift) {
            result.push(ch);
        }
    }
    result
}

fn key_to_char(key: Key, shift: bool) -> Option<char> {
    match key {
        Key::KEY_A => Some(letter('a', shift)),
        Key::KEY_B => Some(letter('b', shift)),
        Key::KEY_C => Some(letter('c', shift)),
        Key::KEY_D => Some(letter('d', shift)),
        Key::KEY_E => Some(letter('e', shift)),
        Key::KEY_F => Some(letter('f', shift)),
        Key::KEY_G => Some(letter('g', shift)),
        Key::KEY_H => Some(letter('h', shift)),
        Key::KEY_I => Some(letter('i', shift)),
        Key::KEY_J => Some(letter('j', shift)),
        Key::KEY_K => Some(letter('k', shift)),
        Key::KEY_L => Some(letter('l', shift)),
        Key::KEY_M => Some(letter('m', shift)),
        Key::KEY_N => Some(letter('n', shift)),
        Key::KEY_O => Some(letter('o', shift)),
        Key::KEY_P => Some(letter('p', shift)),
        Key::KEY_Q => Some(letter('q', shift)),
        Key::KEY_R => Some(letter('r', shift)),
        Key::KEY_S => Some(letter('s', shift)),
        Key::KEY_T => Some(letter('t', shift)),
        Key::KEY_U => Some(letter('u', shift)),
        Key::KEY_V => Some(letter('v', shift)),
        Key::KEY_W => Some(letter('w', shift)),
        Key::KEY_X => Some(letter('x', shift)),
        Key::KEY_Y => Some(letter('y', shift)),
        Key::KEY_Z => Some(letter('z', shift)),
        Key::KEY_0 => Some('0'),
        Key::KEY_1 => Some('1'),
        Key::KEY_2 => Some('2'),
        Key::KEY_3 => Some('3'),
        Key::KEY_4 => Some('4'),
        Key::KEY_5 => Some('5'),
        Key::KEY_6 => Some('6'),
        Key::KEY_7 => Some('7'),
        Key::KEY_8 => Some('8'),
        Key::KEY_9 => Some('9'),
        Key::KEY_SPACE => Some(' '),
        Key::KEY_DOT => Some('.'),
        Key::KEY_COMMA => Some(','),
        Key::KEY_SEMICOLON => Some(';'),
        Key::KEY_APOSTROPHE => Some('\''),
        Key::KEY_GRAVE => Some('`'),
        Key::KEY_LEFTBRACE => Some('['),
        Key::KEY_RIGHTBRACE => Some(']'),
        Key::KEY_SLASH => Some('/'),
        Key::KEY_MINUS => Some('-'),
        Key::KEY_EQUAL => Some('='),
        _ => None,
    }
}

fn letter(base: char, shift: bool) -> char {
    if shift {
        base.to_ascii_uppercase()
    } else {
        base
    }
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
    if eng_vowels >= 2 && clean_word.len() <= 5 {
        return true;
    }

    let common_english_bigrams = [
        "th", "he", "in", "er", "an", "re", "on", "at", "en", "nd", "se", "ed", "te", "st", "el",
        "le", "ti", "io", "ou", "ll", "oo",
    ];
    let common_english_suffixes = ["ed", "ing", "tion", "ment", "ly", "er", "est"];

    if common_english_suffixes
        .iter()
        .any(|suffix| clean_word.ends_with(suffix))
        && eng_vowels >= 2
    {
        return true;
    }

    eng_vowels >= 1
        && common_english_bigrams
            .iter()
            .any(|bigram| clean_word.contains(bigram))
}

fn apply_case_fixes_to_strokes(
    buffer: &[Keystroke],
    fix_two_capitals: bool,
    fix_accidental_caps_lock: bool,
) -> Vec<Keystroke> {
    let Some(pattern) = buffer
        .iter()
        .map(letter_case_for_stroke)
        .collect::<Option<Vec<_>>>()
    else {
        return buffer.to_vec();
    };

    let Some(corrected_pattern) =
        corrected_case_pattern(&pattern, fix_two_capitals, fix_accidental_caps_lock)
    else {
        return buffer.to_vec();
    };

    let mut corrected = buffer.to_vec();
    for (stroke, case) in corrected.iter_mut().zip(corrected_pattern) {
        stroke.shift = matches!(case, LetterCase::Upper);
    }
    corrected
}

#[cfg(test)]
fn apply_case_fixes_to_text(
    word: &str,
    fix_two_capitals: bool,
    fix_accidental_caps_lock: bool,
) -> String {
    let Some(pattern) = word
        .chars()
        .map(letter_case_for_char)
        .collect::<Option<Vec<_>>>()
    else {
        return word.to_string();
    };

    let Some(corrected_pattern) =
        corrected_case_pattern(&pattern, fix_two_capitals, fix_accidental_caps_lock)
    else {
        return word.to_string();
    };

    word.chars()
        .zip(corrected_pattern)
        .flat_map(|(ch, case)| match case {
            LetterCase::Upper => ch.to_uppercase().collect::<Vec<_>>(),
            LetterCase::Lower => ch.to_lowercase().collect::<Vec<_>>(),
        })
        .collect()
}

fn corrected_case_pattern(
    pattern: &[LetterCase],
    fix_two_capitals: bool,
    fix_accidental_caps_lock: bool,
) -> Option<Vec<LetterCase>> {
    if fix_two_capitals
        && pattern.len() >= 3
        && matches!(pattern[0], LetterCase::Upper)
        && matches!(pattern[1], LetterCase::Upper)
        && pattern[2..]
            .iter()
            .all(|case| matches!(case, LetterCase::Lower))
    {
        let mut corrected = pattern.to_vec();
        corrected[1] = LetterCase::Lower;
        return Some(corrected);
    }

    if fix_accidental_caps_lock
        && pattern.len() >= 2
        && matches!(pattern[0], LetterCase::Lower)
        && pattern[1..]
            .iter()
            .all(|case| matches!(case, LetterCase::Upper))
    {
        let mut corrected = pattern.to_vec();
        corrected[0] = LetterCase::Upper;
        for case in &mut corrected[1..] {
            *case = LetterCase::Lower;
        }
        return Some(corrected);
    }

    None
}

fn letter_case_for_stroke(stroke: &Keystroke) -> Option<LetterCase> {
    if !is_case_fix_letter_key(stroke.key) {
        return None;
    }

    Some(if stroke.shift {
        LetterCase::Upper
    } else {
        LetterCase::Lower
    })
}

#[cfg(test)]
fn letter_case_for_char(ch: char) -> Option<LetterCase> {
    if !ch.is_alphabetic() {
        return None;
    }

    if ch.is_uppercase() {
        Some(LetterCase::Upper)
    } else if ch.is_lowercase() {
        Some(LetterCase::Lower)
    } else {
        None
    }
}

fn is_case_fix_letter_key(key: Key) -> bool {
    matches!(
        key,
        Key::KEY_A
            | Key::KEY_B
            | Key::KEY_C
            | Key::KEY_D
            | Key::KEY_E
            | Key::KEY_F
            | Key::KEY_G
            | Key::KEY_H
            | Key::KEY_I
            | Key::KEY_J
            | Key::KEY_K
            | Key::KEY_L
            | Key::KEY_M
            | Key::KEY_N
            | Key::KEY_O
            | Key::KEY_P
            | Key::KEY_Q
            | Key::KEY_R
            | Key::KEY_S
            | Key::KEY_T
            | Key::KEY_U
            | Key::KEY_V
            | Key::KEY_W
            | Key::KEY_X
            | Key::KEY_Y
            | Key::KEY_Z
            | Key::KEY_GRAVE
            | Key::KEY_LEFTBRACE
            | Key::KEY_RIGHTBRACE
            | Key::KEY_SEMICOLON
            | Key::KEY_APOSTROPHE
            | Key::KEY_COMMA
            | Key::KEY_DOT
    )
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

        let plan = manual_correction_plan(&current, &last, true, false, false).unwrap();
        assert_eq!(plan.buffer, current);
        assert_eq!(plan.extra_backspaces, 0);
    }

    #[test]
    fn previous_word_without_separator_does_not_backspace_extra_char() {
        let current = vec![];
        let last = vec![stroke(Key::KEY_F), stroke(Key::KEY_D), stroke(Key::KEY_L)];

        let plan = manual_correction_plan(&current, &last, false, false, false).unwrap();
        assert_eq!(plan.buffer, last);
        assert_eq!(plan.extra_backspaces, 0);
    }

    #[test]
    fn previous_word_with_separator_backspaces_separator_too() {
        let current = vec![];
        let last = vec![stroke(Key::KEY_F), stroke(Key::KEY_D), stroke(Key::KEY_L)];

        let plan = manual_correction_plan(&current, &last, true, false, false).unwrap();
        assert_eq!(plan.buffer, last);
        assert_eq!(plan.extra_backspaces, 1);
    }

    #[test]
    fn fixes_two_capitals_text_pattern_for_russian_and_english_examples() {
        assert_eq!(apply_case_fixes_to_text("ПРивет", true, false), "Привет");
        assert_eq!(apply_case_fixes_to_text("GHbdtn", true, false), "Ghbdtn");
    }

    #[test]
    fn fixes_accidental_caps_lock_text_pattern_for_russian_and_english_examples() {
        assert_eq!(apply_case_fixes_to_text("hELLO", false, true), "Hello");
        assert_eq!(apply_case_fixes_to_text("рУССКИЙ", false, true), "Русский");
    }

    #[test]
    fn leaves_non_matching_words_unchanged() {
        assert_eq!(apply_case_fixes_to_text("NASA", true, true), "NASA");
        assert_eq!(apply_case_fixes_to_text("HeLlo", true, true), "HeLlo");
        assert_eq!(apply_case_fixes_to_text("ПРИВЕТ", true, true), "ПРИВЕТ");
        assert_eq!(apply_case_fixes_to_text("te.st", true, true), "te.st");
    }

    #[test]
    fn manual_plan_applies_case_fix_to_keystrokes() {
        let current = vec![
            Keystroke {
                key: Key::KEY_G,
                shift: true,
            },
            Keystroke {
                key: Key::KEY_H,
                shift: true,
            },
            Keystroke {
                key: Key::KEY_B,
                shift: false,
            },
            Keystroke {
                key: Key::KEY_D,
                shift: false,
            },
            Keystroke {
                key: Key::KEY_T,
                shift: false,
            },
            Keystroke {
                key: Key::KEY_N,
                shift: false,
            },
        ];

        let plan = manual_correction_plan(&current, &[], false, true, false).unwrap();
        assert_eq!(keys_to_string(&plan.buffer), "Ghbdtn");
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

    #[test]
    fn longer_english_word_does_not_trigger_switch() {
        let buffer = vec![
            stroke(Key::KEY_S),
            stroke(Key::KEY_E),
            stroke(Key::KEY_L),
            stroke(Key::KEY_E),
            stroke(Key::KEY_C),
            stroke(Key::KEY_T),
            stroke(Key::KEY_E),
            stroke(Key::KEY_D),
        ];
        assert!(!should_switch(&buffer));
    }

    #[test]
    fn common_short_english_word_does_not_trigger_switch() {
        let buffer = vec![
            stroke(Key::KEY_T),
            stroke(Key::KEY_E),
            stroke(Key::KEY_X),
            stroke(Key::KEY_T),
        ];
        assert!(!should_switch(&buffer));
    }

    #[test]
    fn mixed_case_english_word_does_not_trigger_switch() {
        let buffer = vec![
            Keystroke {
                key: Key::KEY_T,
                shift: true,
            },
            Keystroke {
                key: Key::KEY_E,
                shift: false,
            },
            Keystroke {
                key: Key::KEY_X,
                shift: true,
            },
            Keystroke {
                key: Key::KEY_T,
                shift: false,
            },
        ];

        assert!(!should_switch(&buffer));
    }

    #[test]
    fn mixed_case_english_word_with_trailing_punctuation_does_not_trigger_switch() {
        let buffer = vec![
            Keystroke {
                key: Key::KEY_T,
                shift: true,
            },
            Keystroke {
                key: Key::KEY_E,
                shift: false,
            },
            Keystroke {
                key: Key::KEY_X,
                shift: true,
            },
            Keystroke {
                key: Key::KEY_T,
                shift: false,
            },
            stroke(Key::KEY_DOT),
        ];

        assert!(!should_switch(&buffer));
    }

    #[test]
    fn keys_to_string_uses_direct_character_mapping() {
        let buffer = vec![
            stroke(Key::KEY_C),
            stroke(Key::KEY_A),
            stroke(Key::KEY_R),
            stroke(Key::KEY_DOT),
            stroke(Key::KEY_LEFTBRACE),
        ];

        assert_eq!(keys_to_string(&buffer), "car.[");
    }

    #[test]
    fn keys_to_string_preserves_letter_case_from_shift_state() {
        let buffer = vec![
            Keystroke {
                key: Key::KEY_G,
                shift: false,
            },
            Keystroke {
                key: Key::KEY_H,
                shift: true,
            },
            Keystroke {
                key: Key::KEY_B,
                shift: false,
            },
            Keystroke {
                key: Key::KEY_D,
                shift: true,
            },
            Keystroke {
                key: Key::KEY_T,
                shift: false,
            },
            Keystroke {
                key: Key::KEY_N,
                shift: true,
            },
        ];

        assert_eq!(keys_to_string(&buffer), "gHbDtN");
    }
}
