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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutCorrectionDirection {
    EnglishToRussian,
    RussianToEnglish,
}

// Short command/tool names are treated as English in the EN -> RU guard.
const EXCLUDED_WORDS: &[&str] = &[
    "sudo", "git", "cargo", "rustc", "python", "node", "grep", "echo", "ls", "cd", "rm", "mkdir",
    "apt",
];

// Technical English terms are protected from EN -> RU correction and accepted
// as confident English candidates when typed while the Russian layout is active.
const TECHNICAL_ENGLISH_WORDS: &[&str] = &[
    "api",
    "array",
    "async",
    "await",
    "cache",
    "class",
    "composer",
    "config",
    "css",
    "daemon",
    "debug",
    "dump",
    "error",
    "extends",
    "folder",
    "function",
    "html",
    "http",
    "https",
    "implements",
    "input",
    "interface",
    "json",
    "laravel",
    "layout",
    "middleware",
    "namespace",
    "output",
    "php",
    "private",
    "project",
    "protected",
    "public",
    "queue",
    "request",
    "repository",
    "response",
    "return",
    "screen",
    "service",
    "sql",
    "static",
    "switch",
    "thread",
    "trait",
    "var",
    "worker",
    "xml",
    "yaml",
];

const ENGLISH_LAYOUT_NO_VOWEL_TECHNICAL_WORDS: &[&str] =
    &["ssh", "src", "npm", "pwd", "pdf", "www"];

// Physical-key strings that spell common Russian words have priority over
// English-looking patterns in the RU -> EN heuristic.
const RUSSIAN_PRIORITY_PHYSICAL_WORDS: &[&str] = &[
    "kexit",        // лучше
    "ckexbkjcm",    // случилось
    "ckeifq",       // слушай
    "gjckeifq",     // послушай
    "ckexft",       // случае
    "here",         // руку
    "nhelyj",       // трудно
    "ckexfq",       // случай
    "gjckeifqnt",   // послушайте
    "ckeifqnt",     // слушайте
    "ctreyle",      // секунду
    "ckexbncz",     // случится
    "xedcndetim",   // чувствуешь
    "ckexfqyj",     // случайно
    "ckeifnm",      // слушать
    "ckturf",       // слегка
    "ckexbnmcz",    // случиться
    "ckexftncz",    // случается
    "uhelm",        // грудь
    "ytkturj",      // нелегко
    "gentitcndbt",  // путешествие
    "ctreyljxre",   // секундочку
    "inere",        // штуку
    "gjckeifnm",    // послушать
    "ckeiftim",     // слушаешь
    "eckeue",       // услугу
    "ckeiftn",      // слушает
    "ckexfz",       // случая
    "ckeifk",       // слушал
    "nheljv",       // трудом
    "uhflecjd",     // градусов
    "cjnhelybrjd",  // сотрудников
    "cjnhelybxfnm", // сотрудничать
    "gjkexftim",    // получаешь
    "ckexftd",      // случаев
    "eckeub",       // услуги
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
    layout_correction_direction(buffer, layout_kind).is_some()
}

pub fn layout_correction_direction(
    buffer: &[Keystroke],
    layout_kind: AppLayoutKind,
) -> Option<LayoutCorrectionDirection> {
    match layout_kind {
        AppLayoutKind::English => should_switch_english_to_russian(buffer)
            .then_some(LayoutCorrectionDirection::EnglishToRussian),
        AppLayoutKind::Russian => should_switch_russian_to_english(buffer)
            .then_some(LayoutCorrectionDirection::RussianToEnglish),
        AppLayoutKind::Other | AppLayoutKind::Unknown => None,
    }
}

fn should_switch_english_to_russian(buffer: &[Keystroke]) -> bool {
    let split = split_word_core_and_trailing_tail(buffer, AppLayoutKind::English);
    let core = &split.core;
    if core.len() < 3 {
        return false;
    }

    let word = keys_to_string(core);
    let normalized_word = normalize_word_for_switch_heuristics(&word);
    if is_english_layout_technical_token(core, &normalized_word) {
        return false;
    }
    if is_likely_english(&normalized_word) {
        return false;
    }

    english_layout_russian_physical_score(core, &normalized_word) >= 10
}

fn english_layout_russian_physical_score(core: &[Keystroke], normalized_word: &str) -> i32 {
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

    score
}

fn should_switch_russian_to_english(buffer: &[Keystroke]) -> bool {
    let split = split_physical_english_core_and_trailing_tail(buffer);
    let core = &split.core;
    if core.len() < 3 {
        return false;
    }

    let physical_word = keys_to_string(core);
    let normalized_word = normalize_word_for_switch_heuristics(&physical_word);
    if !normalized_word.chars().all(|ch| ch.is_ascii_alphabetic()) {
        return false;
    }

    let visible_word = keys_to_visible_string(core, AppLayoutKind::Russian);
    if !visible_word.chars().any(is_russian_letter) {
        return false;
    }

    is_confident_english_for_russian_layout(&normalized_word)
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

fn keys_to_visible_string(keys: &[Keystroke], layout_kind: AppLayoutKind) -> String {
    let mut result = String::with_capacity(keys.len());
    for stroke in keys {
        if let Some(ch) = visible_char_for_layout(stroke, layout_kind) {
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

// Replay normalization helpers.

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

// Word splitting / trailing punctuation helpers.

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

fn split_physical_english_core_and_trailing_tail(buffer: &[Keystroke]) -> WordCoreAndTrailingTail {
    let tail_start = buffer
        .iter()
        .rposition(|stroke| {
            !visible_char_for_keystroke(stroke).is_some_and(is_trailing_tail_punctuation_char)
        })
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
        'q' => 'й',
        'Q' => 'Й',
        'w' => 'ц',
        'W' => 'Ц',
        'e' => 'у',
        'E' => 'У',
        'r' => 'к',
        'R' => 'К',
        't' => 'е',
        'T' => 'Е',
        'y' => 'н',
        'Y' => 'Н',
        'u' => 'г',
        'U' => 'Г',
        'i' => 'ш',
        'I' => 'Ш',
        'o' => 'щ',
        'O' => 'Щ',
        'p' => 'з',
        'P' => 'З',
        '`' => 'ё',
        '~' => 'Ё',
        '[' => 'х',
        '{' => 'Х',
        ']' => 'ъ',
        '}' => 'Ъ',
        'a' => 'ф',
        'A' => 'Ф',
        's' => 'ы',
        'S' => 'Ы',
        'd' => 'в',
        'D' => 'В',
        'f' => 'а',
        'F' => 'А',
        'g' => 'п',
        'G' => 'П',
        'h' => 'р',
        'H' => 'Р',
        'j' => 'о',
        'J' => 'О',
        'k' => 'л',
        'K' => 'Л',
        'l' => 'д',
        'L' => 'Д',
        ';' => 'ж',
        ':' => 'Ж',
        '\'' => 'э',
        '"' => 'Э',
        'z' => 'я',
        'Z' => 'Я',
        'x' => 'ч',
        'X' => 'Ч',
        'c' => 'с',
        'C' => 'С',
        'v' => 'м',
        'V' => 'М',
        'b' => 'и',
        'B' => 'И',
        'n' => 'т',
        'N' => 'Т',
        'm' => 'ь',
        'M' => 'Ь',
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

fn is_russian_letter(ch: char) -> bool {
    matches!(ch, 'а'..='я' | 'А'..='Я' | 'ё' | 'Ё')
}

fn contains_latin_vowel(word: &str) -> bool {
    word.chars().any(|ch| "aeiou".contains(ch))
}

fn count_latin_vowels_without_y(word: &str) -> usize {
    word.chars().filter(|ch| "aeiou".contains(*ch)).count()
}

fn is_confident_english_for_russian_layout(word: &str) -> bool {
    if RUSSIAN_PRIORITY_PHYSICAL_WORDS.contains(&word) {
        return false;
    }
    if TECHNICAL_ENGLISH_WORDS.contains(&word) {
        return true;
    }
    if !contains_latin_vowel(word) {
        return false;
    }

    let pattern_score = english_pattern_score(word);
    if word.len() <= 5 {
        return pattern_score >= 3 && !has_short_unlikely_english_cluster(word);
    }

    if has_strong_english_signal(word) && pattern_score >= 2 {
        return true;
    }

    let vowel_count = count_latin_vowels_without_y(word);
    pattern_score >= 4 || (vowel_count >= 2 && pattern_score >= 3 && is_likely_english(word))
}

fn has_strong_english_signal(word: &str) -> bool {
    const STRONG_SUFFIXES: &[&str] = &["ing", "tion", "ment"];
    const STRONG_TRIGRAMS: &[&str] = &[
        "str", "ion", "ent", "ter", "est", "sys", "tem", "key", "fun", "nct", "ret", "tur", "urn",
        "pro", "ect", "swi", "tch",
    ];

    STRONG_SUFFIXES.iter().any(|suffix| word.ends_with(suffix))
        || STRONG_TRIGRAMS.iter().any(|trigram| word.contains(trigram))
}

fn english_pattern_score(word: &str) -> usize {
    const COMMON_BIGRAMS: &[&str] = &[
        "al", "ar", "as", "at", "bo", "br", "ch", "ck", "ct", "do", "ec", "ed", "el", "em", "en",
        "er", "es", "ex", "ey", "fi", "he", "il", "in", "it", "ke", "le", "ll", "lo", "mi", "na",
        "nd", "oa", "on", "ow", "pr", "re", "ro", "se", "st", "sw", "sy", "te", "th", "ti", "wi",
        "ws", "xt", "ys",
    ];
    const COMMON_TRIGRAMS: &[&str] = &[
        "ame", "bro", "cap", "cke", "doc", "ect", "ell", "erm", "est", "hel", "ina", "ing", "ion",
        "ita", "key", "lec", "llo", "min", "nal", "ock", "ows", "pro", "ret", "row", "sel", "ser",
        "swi", "sys", "tal", "tem", "ter", "tch", "tur", "urn",
    ];

    let bigram_score = COMMON_BIGRAMS
        .iter()
        .filter(|bigram| word.contains(**bigram))
        .count();
    let trigram_score = COMMON_TRIGRAMS
        .iter()
        .filter(|trigram| word.contains(**trigram))
        .count()
        * 2;

    bigram_score + trigram_score
}

fn has_short_unlikely_english_cluster(word: &str) -> bool {
    const UNLIKELY_SHORT_CLUSTERS: &[&str] = &["bj", "bn", "bv", "fy", "kf", "lb", "tk", "yu"];

    word.ends_with('f')
        || word.ends_with('j')
        || UNLIKELY_SHORT_CLUSTERS
            .iter()
            .any(|cluster| word.contains(*cluster))
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
    if is_explicit_english_guard_word(clean_word) {
        return true;
    }

    let eng_vowels = clean_word.chars().filter(|c| "aeiouy".contains(*c)).count();
    if eng_vowels >= 2 && clean_word.len() <= 5 {
        return true;
    }

    if eng_vowels >= 2 && has_common_english_suffix(clean_word) {
        return true;
    }

    eng_vowels >= 1 && has_common_english_bigram(clean_word)
}

fn is_explicit_english_guard_word(clean_word: &str) -> bool {
    clean_word.len() < 3
        || EXCLUDED_WORDS.contains(&clean_word)
        || TECHNICAL_ENGLISH_WORDS.contains(&clean_word)
}

fn is_english_layout_technical_token(core: &[Keystroke], normalized_word: &str) -> bool {
    ENGLISH_LAYOUT_NO_VOWEL_TECHNICAL_WORDS.contains(&normalized_word)
        || core.iter().any(is_structural_code_token_stroke)
}

fn is_structural_code_token_stroke(stroke: &Keystroke) -> bool {
    matches!(
        visible_char_for_keystroke(stroke),
        Some('0'..='9' | '/' | ':' | '_' | '.' | '-')
    )
}

fn has_common_english_suffix(clean_word: &str) -> bool {
    const COMMON_ENGLISH_SUFFIXES: &[&str] = &["ed", "ing", "tion", "ment", "ly", "er", "est"];

    COMMON_ENGLISH_SUFFIXES
        .iter()
        .any(|suffix| clean_word.ends_with(suffix))
}

fn has_common_english_bigram(clean_word: &str) -> bool {
    const COMMON_ENGLISH_BIGRAMS: &[&str] = &[
        "th", "he", "in", "er", "an", "re", "on", "at", "en", "nd", "se", "ed", "te", "st", "el",
        "le", "ti", "io", "ou", "ll", "oo",
    ];

    COMMON_ENGLISH_BIGRAMS
        .iter()
        .any(|bigram| clean_word.contains(bigram))
}

// Same-layout case correction helpers.

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

    // Test helpers

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

    fn strokes_for_text(text: &str) -> Vec<Keystroke> {
        text.chars()
            .map(|ch| match ch {
                'a' => stroke(Key::KEY_A),
                'A' => shifted_stroke(Key::KEY_A),
                'b' => stroke(Key::KEY_B),
                'B' => shifted_stroke(Key::KEY_B),
                'c' => stroke(Key::KEY_C),
                'C' => shifted_stroke(Key::KEY_C),
                'd' => stroke(Key::KEY_D),
                'D' => shifted_stroke(Key::KEY_D),
                'e' => stroke(Key::KEY_E),
                'E' => shifted_stroke(Key::KEY_E),
                'f' => stroke(Key::KEY_F),
                'F' => shifted_stroke(Key::KEY_F),
                'g' => stroke(Key::KEY_G),
                'G' => shifted_stroke(Key::KEY_G),
                'h' => stroke(Key::KEY_H),
                'H' => shifted_stroke(Key::KEY_H),
                'i' => stroke(Key::KEY_I),
                'I' => shifted_stroke(Key::KEY_I),
                'j' => stroke(Key::KEY_J),
                'J' => shifted_stroke(Key::KEY_J),
                'k' => stroke(Key::KEY_K),
                'K' => shifted_stroke(Key::KEY_K),
                'l' => stroke(Key::KEY_L),
                'L' => shifted_stroke(Key::KEY_L),
                'm' => stroke(Key::KEY_M),
                'M' => shifted_stroke(Key::KEY_M),
                'n' => stroke(Key::KEY_N),
                'N' => shifted_stroke(Key::KEY_N),
                'o' => stroke(Key::KEY_O),
                'O' => shifted_stroke(Key::KEY_O),
                'p' => stroke(Key::KEY_P),
                'P' => shifted_stroke(Key::KEY_P),
                'q' => stroke(Key::KEY_Q),
                'Q' => shifted_stroke(Key::KEY_Q),
                'r' => stroke(Key::KEY_R),
                'R' => shifted_stroke(Key::KEY_R),
                's' => stroke(Key::KEY_S),
                'S' => shifted_stroke(Key::KEY_S),
                't' => stroke(Key::KEY_T),
                'T' => shifted_stroke(Key::KEY_T),
                'u' => stroke(Key::KEY_U),
                'U' => shifted_stroke(Key::KEY_U),
                'v' => stroke(Key::KEY_V),
                'V' => shifted_stroke(Key::KEY_V),
                'w' => stroke(Key::KEY_W),
                'W' => shifted_stroke(Key::KEY_W),
                'x' => stroke(Key::KEY_X),
                'X' => shifted_stroke(Key::KEY_X),
                'y' => stroke(Key::KEY_Y),
                'Y' => shifted_stroke(Key::KEY_Y),
                'z' => stroke(Key::KEY_Z),
                'Z' => shifted_stroke(Key::KEY_Z),
                '0' => stroke(Key::KEY_0),
                '1' => stroke(Key::KEY_1),
                '2' => stroke(Key::KEY_2),
                '3' => stroke(Key::KEY_3),
                '4' => stroke(Key::KEY_4),
                '5' => stroke(Key::KEY_5),
                '6' => stroke(Key::KEY_6),
                '7' => stroke(Key::KEY_7),
                '8' => stroke(Key::KEY_8),
                '9' => stroke(Key::KEY_9),
                ',' => stroke(Key::KEY_COMMA),
                '.' => stroke(Key::KEY_DOT),
                '/' => stroke(Key::KEY_SLASH),
                ':' => shifted_stroke(Key::KEY_SEMICOLON),
                ';' => stroke(Key::KEY_SEMICOLON),
                '-' => stroke(Key::KEY_MINUS),
                '_' => shifted_stroke(Key::KEY_MINUS),
                '!' => shifted_stroke(Key::KEY_1),
                '?' => shifted_stroke(Key::KEY_SLASH),
                _ => panic!("unsupported test character: {ch}"),
            })
            .collect()
    }

    fn assert_english_layout_tokens_do_not_switch(tokens: &[&str]) {
        let triggered = tokens
            .iter()
            .copied()
            .filter(|token| should_switch(&strokes_for_text(token), AppLayoutKind::English))
            .collect::<Vec<_>>();
        assert!(
            triggered.is_empty(),
            "tokens must not trigger EN -> RU correction: {triggered:?}"
        );
    }

    // Word splitting / trailing punctuation

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

    // Manual correction plans

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

    // Same-layout case correction and case preservation

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

    // EN -> RU auto-switch heuristic

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
    fn english_layout_russian_physical_words_trigger_switch() {
        for word in [
            "ghbdtn",
            "gjrf",
            "vfvf",
            "rjn",
            "ltkf",
            "ghjuhfvvf",
            "yfcnjqrb",
            "ctvz",
            "gjckt",
            "xnj,s",
        ] {
            let buffer = strokes_for_text(word);
            assert!(
                should_switch(&buffer, AppLayoutKind::English),
                "{word} must trigger EN -> RU correction"
            );
        }
    }

    #[test]
    fn english_layout_common_and_technical_words_do_not_trigger_switch() {
        for word in [
            "hello", "selected", "docker", "terminal", "config", "browser", "cargo", "rust",
            "sudo", "git",
        ] {
            let buffer = strokes_for_text(word);
            assert!(
                !should_switch(&buffer, AppLayoutKind::English),
                "{word} must not trigger EN -> RU correction"
            );
        }
    }

    #[test]
    fn english_layout_code_like_tokens_do_not_trigger_switch() {
        for token in [
            "snake_case",
            "camelCase",
            "http",
            "https",
            "localhost",
            "config.toml",
        ] {
            let buffer = strokes_for_text(token);
            assert!(
                !should_switch(&buffer, AppLayoutKind::English),
                "{token} must not trigger EN -> RU correction"
            );
        }
    }

    #[test]
    fn english_layout_short_technical_no_vowel_tokens_do_not_trigger_switch() {
        assert_english_layout_tokens_do_not_switch(&["ssh", "src", "npm", "pwd", "pdf", "www"]);
    }

    #[test]
    fn english_layout_code_and_path_like_tokens_do_not_trigger_switch() {
        assert_english_layout_tokens_do_not_switch(&[
            "id_rsa",
            "php8",
            "src/lib",
            "http://",
            "localhost:3000",
            "config.json",
            "src/main.rs",
        ]);
    }

    #[test]
    fn english_layout_punctuation_corpus_matches_expected_correction_behavior() {
        for word in ["ghbdtn;", "ghbdtn:", "ghbdtn?!"] {
            let buffer = strokes_for_text(word);
            assert!(
                should_switch(&buffer, AppLayoutKind::English),
                "{word} must trigger EN -> RU correction"
            );
        }

        for punctuation in ["!!!", "?!"] {
            let buffer = strokes_for_text(punctuation);
            assert!(
                !should_switch(&buffer, AppLayoutKind::English),
                "{punctuation} must not trigger EN -> RU correction"
            );
        }
    }

    // RU -> EN auto-switch heuristic

    #[test]
    fn russian_layout_english_word_hello_triggers_switch() {
        let buffer = strokes_for_text("hello");
        assert!(should_switch(&buffer, AppLayoutKind::Russian));
    }

    #[test]
    fn russian_layout_english_word_test_triggers_switch() {
        let buffer = strokes_for_text("test");
        assert!(should_switch(&buffer, AppLayoutKind::Russian));
    }

    #[test]
    fn russian_layout_english_word_selected_triggers_switch() {
        let buffer = strokes_for_text("selected");
        assert!(should_switch(&buffer, AppLayoutKind::Russian));
    }

    #[test]
    fn russian_layout_common_english_words_trigger_switch() {
        for word in [
            "text", "browser", "docker", "terminal", "keyboard", "project", "switch", "screen",
            "folder", "daemon", "layout", "input", "output", "error", "debug", "config", "cache",
            "queue", "worker", "thread", "async", "await", "name", "capital",
        ] {
            let buffer = strokes_for_text(word);
            assert!(
                should_switch(&buffer, AppLayoutKind::Russian),
                "{word} must trigger RU -> EN correction"
            );
        }
    }

    #[test]
    fn russian_layout_correct_russian_privet_does_not_trigger_switch() {
        let buffer = strokes_for_text("ghbdtn");
        assert!(!should_switch(&buffer, AppLayoutKind::Russian));
    }

    #[test]
    fn russian_layout_correct_russian_mama_does_not_trigger_switch() {
        let buffer = strokes_for_text("vfvf");
        assert!(!should_switch(&buffer, AppLayoutKind::Russian));
    }

    #[test]
    fn russian_layout_correct_russian_kot_does_not_trigger_switch() {
        let buffer = strokes_for_text("rjn");
        assert!(!should_switch(&buffer, AppLayoutKind::Russian));
    }

    #[test]
    fn russian_layout_correct_russian_naprimer_does_not_trigger_switch() {
        let buffer = strokes_for_text("yfghbvth");
        assert!(!should_switch(&buffer, AppLayoutKind::Russian));
    }

    #[test]
    fn russian_layout_correct_russian_common_words_do_not_trigger_switch() {
        for (physical, russian) in [
            ("cltkftim", "сделаешь"),
            ("ltkftim", "делаешь"),
            ("ctujlyz", "сегодня"),
            ("gjxtve", "почему"),
            ("ghjuhfvvf", "программа"),
            ("rjnjhsq", "который"),
            ("lfyyst", "данные"),
            ("cltkfk", "сделал"),
            ("frekf", "акула"),
            ("fyutk", "ангел"),
            ("felbj", "аудио"),
            ("felbn", "аудит"),
            ("kexit", "лучше"),
            ("here", "руку"),
            ("ckexfq", "случай"),
            ("ckeifq", "слушай"),
            ("uhelm", "грудь"),
            ("eckeub", "услуги"),
        ] {
            let buffer = strokes_for_text(physical);
            assert!(
                !should_switch(&buffer, AppLayoutKind::Russian),
                "{russian} ({physical}) must keep Russian priority"
            );
        }
    }

    #[test]
    fn russian_layout_php_programming_words_trigger_switch() {
        for word in [
            "php",
            "class",
            "function",
            "array",
            "namespace",
            "composer",
            "laravel",
            "public",
            "private",
            "protected",
            "return",
            "string",
            "interface",
            "trait",
            "extends",
            "implements",
            "static",
            "json",
            "request",
            "response",
            "controller",
            "service",
            "repository",
            "middleware",
        ] {
            let buffer = strokes_for_text(word);
            assert!(
                should_switch(&buffer, AppLayoutKind::Russian),
                "{word} must trigger RU -> EN correction"
            );
        }
    }

    #[test]
    fn russian_layout_short_technical_tokens_document_current_false_negative_behavior() {
        for word in ["cargo", "rust", "sudo", "git", "ssh", "npm", "jwt"] {
            let buffer = strokes_for_text(word);
            assert!(
                !should_switch(&buffer, AppLayoutKind::Russian),
                "{word} is currently accepted as a RU -> EN false negative"
            );
        }
    }

    #[test]
    fn russian_layout_short_word_does_not_trigger_switch() {
        let buffer = strokes_for_text("hi");
        assert!(!should_switch(&buffer, AppLayoutKind::Russian));
    }

    #[test]
    fn russian_layout_punctuation_only_does_not_trigger_switch() {
        let buffer = vec![
            stroke(Key::KEY_SLASH),
            shifted_stroke(Key::KEY_SLASH),
            shifted_stroke(Key::KEY_1),
        ];
        assert!(!should_switch(&buffer, AppLayoutKind::Russian));
    }

    #[test]
    fn russian_layout_numbers_and_symbols_only_do_not_trigger_switch() {
        let buffer = vec![
            stroke(Key::KEY_1),
            stroke(Key::KEY_2),
            shifted_stroke(Key::KEY_3),
        ];
        assert!(!should_switch(&buffer, AppLayoutKind::Russian));
    }

    #[test]
    fn russian_layout_english_word_with_trailing_punctuation_triggers_switch() {
        let buffer = strokes_for_text("hello.");
        assert!(should_switch(&buffer, AppLayoutKind::Russian));
    }

    #[test]
    fn russian_layout_system_with_physical_period_triggers_switch() {
        let buffer = strokes_for_text("system.");
        assert!(should_switch(&buffer, AppLayoutKind::Russian));
    }

    #[test]
    fn russian_layout_title_case_english_word_keeps_replay_case() {
        let buffer = strokes_for_text("Hello");
        assert!(should_switch(&buffer, AppLayoutKind::Russian));

        let plan = manual_correction_plan(&buffer, &[], false, AppLayoutKind::Russian).unwrap();
        assert_eq!(keys_to_string(&plan.buffer), "Hello");
    }

    #[test]
    fn russian_layout_uppercase_english_word_keeps_replay_case() {
        let buffer = strokes_for_text("HELLO");
        assert!(should_switch(&buffer, AppLayoutKind::Russian));

        let plan = manual_correction_plan(&buffer, &[], false, AppLayoutKind::Russian).unwrap();
        assert_eq!(keys_to_string(&plan.buffer), "HELLO");
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

    // False positives / technical tokens

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

    // Low-level key/string helpers

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
