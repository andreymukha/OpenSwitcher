use crate::layout_backend::AppLayoutKind;
use evdev::Key;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keystroke {
    pub key: Key,
    pub shift: bool,
    pub caps_lock: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorrectionPlan {
    pub buffer: Vec<Keystroke>,
    pub extra_backspaces: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WordCoreAndTrailingTail {
    core: Vec<Keystroke>,
    trailing_tail: Vec<Keystroke>,
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
    layout_kind: AppLayoutKind,
) -> Option<CorrectionPlan> {
    let (buffer, extra_backspaces) = if !current_buffer.is_empty() {
        (current_buffer, 0)
    } else if !last_word_buffer.is_empty() {
        (
            last_word_buffer,
            usize::from(last_word_followed_by_separator),
        )
    } else {
        return None;
    };

    let split = split_word_core_and_trailing_tail(buffer, layout_kind);
    if split.core.is_empty() {
        return None;
    }

    let mut normalized = normalized_replay_buffer(&split.core);
    normalized.extend(normalized_replay_buffer(&split.trailing_tail));
    Some(CorrectionPlan {
        buffer: normalized,
        extra_backspaces,
    })
}

pub fn same_layout_case_correction_plan(
    buffer: &[Keystroke],
    layout_kind: AppLayoutKind,
    fix_two_capitals: bool,
    fix_accidental_caps_lock: bool,
) -> Option<CorrectionPlan> {
    let split = split_word_core_and_trailing_tail(buffer, layout_kind);
    if split.core.is_empty() {
        return None;
    }

    let normalized_core = normalized_replay_buffer(&split.core);
    let corrected_core =
        apply_case_fixes_to_strokes(&split.core, fix_two_capitals, fix_accidental_caps_lock);
    if corrected_core == normalized_core {
        return None;
    }

    let mut corrected = corrected_core;
    corrected.extend(normalized_replay_buffer(&split.trailing_tail));
    Some(CorrectionPlan {
        buffer: corrected,
        extra_backspaces: 0,
    })
}

pub fn should_switch(buffer: &[Keystroke], layout_kind: AppLayoutKind) -> bool {
    let split = split_word_core_and_trailing_tail(buffer, layout_kind);
    let core = &split.core;
    if core.len() < 3 {
        return false;
    }

    let word = keys_to_string(core);
    let normalized_word = normalize_word_for_switch_heuristics(&word);
    if is_likely_english(&normalized_word) {
        return false;
    }

    let mut score = 0;

    for (index, key) in core.iter().enumerate() {
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
            score += if index < core.len() - 1 { 15 } else { 5 };
        }
    }

    let rus_vowels = count_russian_vowels(core);
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

pub fn visible_char_for_keystroke(stroke: &Keystroke) -> Option<char> {
    key_to_char(stroke.key, stroke.shift, stroke.caps_lock)
}

fn keys_to_string(keys: &[Keystroke]) -> String {
    let mut result = String::with_capacity(keys.len());
    for stroke in keys {
        if let Some(ch) = visible_char_for_keystroke(stroke) {
            result.push(ch);
        }
    }
    result
}

fn key_to_char(key: Key, shift: bool, caps_lock: bool) -> Option<char> {
    match key {
        Key::KEY_A => Some(letter('a', shift, caps_lock)),
        Key::KEY_B => Some(letter('b', shift, caps_lock)),
        Key::KEY_C => Some(letter('c', shift, caps_lock)),
        Key::KEY_D => Some(letter('d', shift, caps_lock)),
        Key::KEY_E => Some(letter('e', shift, caps_lock)),
        Key::KEY_F => Some(letter('f', shift, caps_lock)),
        Key::KEY_G => Some(letter('g', shift, caps_lock)),
        Key::KEY_H => Some(letter('h', shift, caps_lock)),
        Key::KEY_I => Some(letter('i', shift, caps_lock)),
        Key::KEY_J => Some(letter('j', shift, caps_lock)),
        Key::KEY_K => Some(letter('k', shift, caps_lock)),
        Key::KEY_L => Some(letter('l', shift, caps_lock)),
        Key::KEY_M => Some(letter('m', shift, caps_lock)),
        Key::KEY_N => Some(letter('n', shift, caps_lock)),
        Key::KEY_O => Some(letter('o', shift, caps_lock)),
        Key::KEY_P => Some(letter('p', shift, caps_lock)),
        Key::KEY_Q => Some(letter('q', shift, caps_lock)),
        Key::KEY_R => Some(letter('r', shift, caps_lock)),
        Key::KEY_S => Some(letter('s', shift, caps_lock)),
        Key::KEY_T => Some(letter('t', shift, caps_lock)),
        Key::KEY_U => Some(letter('u', shift, caps_lock)),
        Key::KEY_V => Some(letter('v', shift, caps_lock)),
        Key::KEY_W => Some(letter('w', shift, caps_lock)),
        Key::KEY_X => Some(letter('x', shift, caps_lock)),
        Key::KEY_Y => Some(letter('y', shift, caps_lock)),
        Key::KEY_Z => Some(letter('z', shift, caps_lock)),
        Key::KEY_0 => Some(if shift { ')' } else { '0' }),
        Key::KEY_1 => Some(if shift { '!' } else { '1' }),
        Key::KEY_2 => Some(if shift { '@' } else { '2' }),
        Key::KEY_3 => Some(if shift { '#' } else { '3' }),
        Key::KEY_4 => Some(if shift { '$' } else { '4' }),
        Key::KEY_5 => Some(if shift { '%' } else { '5' }),
        Key::KEY_6 => Some(if shift { '^' } else { '6' }),
        Key::KEY_7 => Some(if shift { '&' } else { '7' }),
        Key::KEY_8 => Some(if shift { '*' } else { '8' }),
        Key::KEY_9 => Some(if shift { '(' } else { '9' }),
        Key::KEY_SPACE => Some(' '),
        Key::KEY_DOT => Some(if shift { '>' } else { '.' }),
        Key::KEY_COMMA => Some(if shift { '<' } else { ',' }),
        Key::KEY_SEMICOLON => Some(if shift { ':' } else { ';' }),
        Key::KEY_APOSTROPHE => Some(if shift { '"' } else { '\'' }),
        Key::KEY_GRAVE => Some(if shift { '~' } else { '`' }),
        Key::KEY_LEFTBRACE => Some(if shift { '{' } else { '[' }),
        Key::KEY_RIGHTBRACE => Some(if shift { '}' } else { ']' }),
        Key::KEY_SLASH => Some(if shift { '?' } else { '/' }),
        Key::KEY_MINUS => Some(if shift { '_' } else { '-' }),
        Key::KEY_EQUAL => Some(if shift { '+' } else { '=' }),
        _ => None,
    }
}

fn letter(base: char, shift: bool, caps_lock: bool) -> char {
    if shift ^ caps_lock {
        base.to_ascii_uppercase()
    } else {
        base
    }
}

fn normalize_strokes_for_replay(buffer: &[Keystroke]) -> Vec<Keystroke> {
    buffer
        .iter()
        .map(|stroke| match letter_case_for_stroke(stroke) {
            Some(LetterCase::Upper) => Keystroke {
                key: stroke.key,
                shift: true,
                caps_lock: false,
            },
            Some(LetterCase::Lower) => Keystroke {
                key: stroke.key,
                shift: false,
                caps_lock: false,
            },
            None => Keystroke {
                key: stroke.key,
                shift: stroke.shift,
                caps_lock: false,
            },
        })
        .collect()
}

fn normalized_replay_buffer(buffer: &[Keystroke]) -> Vec<Keystroke> {
    normalize_strokes_for_replay(buffer)
}

fn split_word_core_and_trailing_tail(
    buffer: &[Keystroke],
    layout_kind: AppLayoutKind,
) -> WordCoreAndTrailingTail {
    let tail_start = buffer
        .iter()
        .rposition(|stroke| !is_trailing_tail_punctuation(stroke, layout_kind))
        .map_or(0, |index| index + 1);

    WordCoreAndTrailingTail {
        core: buffer[..tail_start].to_vec(),
        trailing_tail: buffer[tail_start..].to_vec(),
    }
}

fn is_trailing_tail_punctuation(stroke: &Keystroke, layout_kind: AppLayoutKind) -> bool {
    visible_char_for_layout(stroke, layout_kind).is_some_and(is_trailing_tail_punctuation_char)
}

fn is_trailing_tail_punctuation_char(ch: char) -> bool {
    matches!(ch, ',' | '.' | ';' | ':' | '!' | '?')
}

fn visible_char_for_layout(stroke: &Keystroke, layout_kind: AppLayoutKind) -> Option<char> {
    let english_visible_char = visible_char_for_keystroke(stroke)?;
    Some(match layout_kind {
        AppLayoutKind::English | AppLayoutKind::Other | AppLayoutKind::Unknown => {
            english_visible_char
        }
        AppLayoutKind::Russian => russian_visible_char_for_keyboard_position(english_visible_char),
    })
}

fn russian_visible_char_for_keyboard_position(ch: char) -> char {
    match ch {
        '`' => 'ё',
        '~' => 'Ё',
        '[' => 'х',
        '{' => 'Х',
        ']' => 'ъ',
        '}' => 'Ъ',
        ';' => 'ж',
        ':' => 'Ж',
        '\'' => 'э',
        '"' => 'Э',
        ',' => 'б',
        '<' => 'Б',
        '.' => 'ю',
        '>' => 'Ю',
        '/' => '.',
        '?' => ',',
        '\\' => '\\',
        '|' => '/',
        _ => ch,
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

pub fn apply_case_fixes_to_strokes(
    buffer: &[Keystroke],
    fix_two_capitals: bool,
    fix_accidental_caps_lock: bool,
) -> Vec<Keystroke> {
    let normalized = normalize_strokes_for_replay(buffer);
    let Some(pattern) = buffer
        .iter()
        .map(letter_case_for_stroke)
        .collect::<Option<Vec<_>>>()
    else {
        return normalized;
    };

    let Some(corrected_pattern) =
        corrected_case_pattern(&pattern, fix_two_capitals, fix_accidental_caps_lock)
    else {
        return normalized;
    };

    let mut corrected = normalized;
    for (stroke, case) in corrected.iter_mut().zip(corrected_pattern) {
        stroke.shift = matches!(case, LetterCase::Upper);
        stroke.caps_lock = false;
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

    Some(if stroke.shift ^ stroke.caps_lock {
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
    use crate::layout_backend::AppLayoutKind;

    fn stroke(key: Key) -> Keystroke {
        Keystroke {
            key,
            shift: false,
            caps_lock: false,
        }
    }

    fn shifted_stroke(key: Key) -> Keystroke {
        Keystroke {
            key,
            shift: true,
            caps_lock: false,
        }
    }

    #[test]
    fn split_trailing_tail_extracts_english_comma_suffix() {
        let buffer = vec![
            stroke(Key::KEY_G),
            stroke(Key::KEY_H),
            stroke(Key::KEY_B),
            stroke(Key::KEY_D),
            stroke(Key::KEY_T),
            stroke(Key::KEY_N),
            stroke(Key::KEY_COMMA),
        ];

        let split = split_word_core_and_trailing_tail(&buffer, AppLayoutKind::English);
        assert_eq!(keys_to_string(&split.core), "ghbdtn");
        assert_eq!(keys_to_string(&split.trailing_tail), ",");
    }

    #[test]
    fn split_trailing_tail_keeps_internal_punctuation_in_core() {
        let buffer = vec![
            stroke(Key::KEY_N),
            stroke(Key::KEY_T),
            stroke(Key::KEY_COMMA),
            stroke(Key::KEY_Z),
        ];

        let split = split_word_core_and_trailing_tail(&buffer, AppLayoutKind::English);
        assert_eq!(keys_to_string(&split.core), "nt,z");
        assert!(split.trailing_tail.is_empty());
    }

    #[test]
    fn split_trailing_tail_extracts_repeated_suffix() {
        let buffer = vec![
            stroke(Key::KEY_H),
            stroke(Key::KEY_E),
            stroke(Key::KEY_L),
            stroke(Key::KEY_L),
            stroke(Key::KEY_O),
            stroke(Key::KEY_DOT),
            stroke(Key::KEY_DOT),
            stroke(Key::KEY_DOT),
        ];

        let split = split_word_core_and_trailing_tail(&buffer, AppLayoutKind::English);
        assert_eq!(keys_to_string(&split.core), "hello");
        assert_eq!(keys_to_string(&split.trailing_tail), "...");
    }

    #[test]
    fn split_trailing_tail_extracts_russian_period_from_slash_key() {
        let buffer = vec![
            stroke(Key::KEY_G),
            stroke(Key::KEY_H),
            stroke(Key::KEY_B),
            stroke(Key::KEY_D),
            stroke(Key::KEY_T),
            stroke(Key::KEY_N),
            stroke(Key::KEY_SLASH),
        ];

        let split = split_word_core_and_trailing_tail(&buffer, AppLayoutKind::Russian);
        assert_eq!(keys_to_string(&split.core), "ghbdtn");
        assert_eq!(split.trailing_tail, vec![stroke(Key::KEY_SLASH)]);
    }

    #[test]
    fn split_trailing_tail_does_not_move_apostrophe_into_tail() {
        let buffer = vec![
            stroke(Key::KEY_H),
            stroke(Key::KEY_E),
            stroke(Key::KEY_APOSTROPHE),
        ];

        let split = split_word_core_and_trailing_tail(&buffer, AppLayoutKind::English);
        assert_eq!(keys_to_string(&split.core), "he'");
        assert!(split.trailing_tail.is_empty());
    }

    #[test]
    fn manual_plan_prefers_current_buffer() {
        let current = vec![stroke(Key::KEY_F), stroke(Key::KEY_D), stroke(Key::KEY_L)];
        let last = vec![stroke(Key::KEY_A)];

        let plan = manual_correction_plan(&current, &last, true, AppLayoutKind::English).unwrap();
        assert_eq!(plan.buffer, current);
        assert_eq!(plan.extra_backspaces, 0);
    }

    #[test]
    fn previous_word_without_separator_does_not_backspace_extra_char() {
        let current = vec![];
        let last = vec![stroke(Key::KEY_F), stroke(Key::KEY_D), stroke(Key::KEY_L)];

        let plan = manual_correction_plan(&current, &last, false, AppLayoutKind::English).unwrap();
        assert_eq!(plan.buffer, last);
        assert_eq!(plan.extra_backspaces, 0);
    }

    #[test]
    fn previous_word_with_separator_backspaces_separator_too() {
        let current = vec![];
        let last = vec![stroke(Key::KEY_F), stroke(Key::KEY_D), stroke(Key::KEY_L)];

        let plan = manual_correction_plan(&current, &last, true, AppLayoutKind::English).unwrap();
        assert_eq!(plan.buffer, last);
        assert_eq!(plan.extra_backspaces, 1);
    }

    #[test]
    fn manual_plan_preserves_trailing_tail_for_previous_word() {
        let current = vec![];
        let last = vec![
            stroke(Key::KEY_F),
            stroke(Key::KEY_D),
            stroke(Key::KEY_L),
            stroke(Key::KEY_COMMA),
        ];

        let plan = manual_correction_plan(&current, &last, true, AppLayoutKind::English).unwrap();
        assert_eq!(keys_to_string(&plan.buffer), "fdl,");
        assert_eq!(plan.extra_backspaces, 1);
    }

    #[test]
    fn manual_plan_returns_none_for_punctuation_only_buffer() {
        let current = vec![stroke(Key::KEY_DOT), stroke(Key::KEY_DOT)];

        assert!(manual_correction_plan(&current, &[], false, AppLayoutKind::English).is_none());
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
    fn same_layout_case_fix_plan_applies_case_fix_to_keystrokes() {
        let current = vec![
            Keystroke {
                key: Key::KEY_G,
                shift: true,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_H,
                shift: true,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_B,
                shift: false,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_D,
                shift: false,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_T,
                shift: false,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_N,
                shift: false,
                caps_lock: false,
            },
        ];

        let plan = same_layout_case_correction_plan(&current, AppLayoutKind::English, true, false)
            .unwrap();
        assert_eq!(keys_to_string(&plan.buffer), "Ghbdtn");
    }

    #[test]
    fn same_layout_case_fix_plan_preserves_question_tail() {
        let buffer = vec![
            Keystroke {
                key: Key::KEY_H,
                shift: false,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_E,
                shift: true,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_L,
                shift: true,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_L,
                shift: true,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_O,
                shift: true,
                caps_lock: false,
            },
            shifted_stroke(Key::KEY_SLASH),
        ];

        let plan =
            same_layout_case_correction_plan(&buffer, AppLayoutKind::English, false, true).unwrap();
        assert_eq!(keys_to_string(&plan.buffer), "Hello?");
    }

    #[test]
    fn same_layout_case_fix_plan_preserves_exclamation_tail() {
        let buffer = vec![
            Keystroke {
                key: Key::KEY_H,
                shift: false,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_E,
                shift: true,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_L,
                shift: true,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_L,
                shift: true,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_O,
                shift: true,
                caps_lock: false,
            },
            shifted_stroke(Key::KEY_1),
        ];

        let plan =
            same_layout_case_correction_plan(&buffer, AppLayoutKind::English, false, true).unwrap();
        assert_eq!(plan.buffer.last(), Some(&shifted_stroke(Key::KEY_1)));
    }

    #[test]
    fn layout_correction_plan_preserves_original_case_before_case_fix() {
        let current = vec![
            Keystroke {
                key: Key::KEY_G,
                shift: true,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_H,
                shift: true,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_B,
                shift: false,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_D,
                shift: false,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_T,
                shift: false,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_N,
                shift: false,
                caps_lock: false,
            },
        ];

        let plan = manual_correction_plan(&current, &[], false, AppLayoutKind::English).unwrap();
        assert_eq!(keys_to_string(&plan.buffer), "GHbdtn");

        let corrected = apply_case_fixes_to_strokes(&plan.buffer, true, false);
        assert_eq!(keys_to_string(&corrected), "Ghbdtn");
    }

    #[test]
    fn russian_like_word_triggers_switch() {
        let buffer = vec![stroke(Key::KEY_F), stroke(Key::KEY_D), stroke(Key::KEY_L)];
        assert!(should_switch(&buffer, AppLayoutKind::English));
    }

    #[test]
    fn russian_like_word_with_trailing_punctuation_triggers_switch() {
        let buffer = vec![
            stroke(Key::KEY_F),
            stroke(Key::KEY_D),
            stroke(Key::KEY_L),
            stroke(Key::KEY_COMMA),
        ];
        assert!(should_switch(&buffer, AppLayoutKind::English));
    }

    #[test]
    fn same_layout_case_fix_handles_physical_caps_lock_pattern() {
        let buffer = vec![
            Keystroke {
                key: Key::KEY_H,
                shift: true,
                caps_lock: true,
            },
            Keystroke {
                key: Key::KEY_E,
                shift: false,
                caps_lock: true,
            },
            Keystroke {
                key: Key::KEY_L,
                shift: false,
                caps_lock: true,
            },
            Keystroke {
                key: Key::KEY_L,
                shift: false,
                caps_lock: true,
            },
            Keystroke {
                key: Key::KEY_O,
                shift: false,
                caps_lock: true,
            },
        ];

        assert_eq!(keys_to_string(&buffer), "hELLO");

        let plan =
            same_layout_case_correction_plan(&buffer, AppLayoutKind::English, false, true).unwrap();
        assert_eq!(keys_to_string(&plan.buffer), "Hello");
    }

    #[test]
    fn common_english_word_does_not_trigger_switch() {
        let buffer = vec![stroke(Key::KEY_C), stroke(Key::KEY_A), stroke(Key::KEY_R)];
        assert!(!should_switch(&buffer, AppLayoutKind::English));
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
        assert!(!should_switch(&buffer, AppLayoutKind::English));
    }

    #[test]
    fn common_short_english_word_does_not_trigger_switch() {
        let buffer = vec![
            stroke(Key::KEY_T),
            stroke(Key::KEY_E),
            stroke(Key::KEY_X),
            stroke(Key::KEY_T),
        ];
        assert!(!should_switch(&buffer, AppLayoutKind::English));
    }

    #[test]
    fn mixed_case_english_word_does_not_trigger_switch() {
        let buffer = vec![
            Keystroke {
                key: Key::KEY_T,
                shift: true,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_E,
                shift: false,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_X,
                shift: true,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_T,
                shift: false,
                caps_lock: false,
            },
        ];

        assert!(!should_switch(&buffer, AppLayoutKind::English));
    }

    #[test]
    fn mixed_case_english_word_with_trailing_punctuation_does_not_trigger_switch() {
        let buffer = vec![
            Keystroke {
                key: Key::KEY_T,
                shift: true,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_E,
                shift: false,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_X,
                shift: true,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_T,
                shift: false,
                caps_lock: false,
            },
            stroke(Key::KEY_DOT),
        ];

        assert!(!should_switch(&buffer, AppLayoutKind::English));
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
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_H,
                shift: true,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_B,
                shift: false,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_D,
                shift: true,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_T,
                shift: false,
                caps_lock: false,
            },
            Keystroke {
                key: Key::KEY_N,
                shift: true,
                caps_lock: false,
            },
        ];

        assert_eq!(keys_to_string(&buffer), "gHbDtN");
    }
}
