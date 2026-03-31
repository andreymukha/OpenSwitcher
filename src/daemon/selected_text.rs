use crate::daemon::keyboard::{KeyboardController, ModifierState};
use crate::error::SelectedTextError;
use arboard::Clipboard;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const COPY_SETTLE_DELAY: Duration = Duration::from_millis(120);
const PASTE_SETTLE_DELAY: Duration = Duration::from_millis(120);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversionDirection {
    EnToRu,
    RuToEn,
    Mixed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SelectedTextSwitchResult {
    Replaced {
        direction: ConversionDirection,
        clipboard_restored: bool,
    },
    NoSelectedText,
    NotConvertible,
}

trait ClipboardAccess {
    fn get_text(&mut self) -> Result<String, SelectedTextError>;
    fn set_text(&mut self, value: &str) -> Result<(), SelectedTextError>;
}

trait SelectionTransport {
    fn copy_selection(
        &mut self,
        modifiers: ModifierState,
    ) -> Result<(), crate::error::SwitcherError>;
    fn paste_selection(
        &mut self,
        modifiers: ModifierState,
    ) -> Result<(), crate::error::SwitcherError>;
}

struct SystemClipboard {
    inner: Clipboard,
}

impl SystemClipboard {
    fn new() -> Result<Self, SelectedTextError> {
        Clipboard::new()
            .map(|inner| Self { inner })
            .map_err(SelectedTextError::ClipboardUnavailable)
    }
}

impl ClipboardAccess for SystemClipboard {
    fn get_text(&mut self) -> Result<String, SelectedTextError> {
        self.inner
            .get_text()
            .map_err(SelectedTextError::ClipboardRead)
    }

    fn set_text(&mut self, value: &str) -> Result<(), SelectedTextError> {
        self.inner
            .set_text(value.to_string())
            .map_err(SelectedTextError::ClipboardWrite)
    }
}

impl SelectionTransport for KeyboardController {
    fn copy_selection(
        &mut self,
        modifiers: ModifierState,
    ) -> Result<(), crate::error::SwitcherError> {
        self.send_copy_shortcut(modifiers)
    }

    fn paste_selection(
        &mut self,
        modifiers: ModifierState,
    ) -> Result<(), crate::error::SwitcherError> {
        self.send_paste_shortcut(modifiers)
    }
}

#[derive(Default)]
pub struct SelectedTextSwitchService;

impl SelectedTextSwitchService {
    pub fn switch_selected_text(
        &self,
        keyboard: &mut KeyboardController,
        modifiers: ModifierState,
    ) -> Result<SelectedTextSwitchResult, crate::error::SwitcherError> {
        let mut clipboard = SystemClipboard::new()?;
        self.switch_with_backends(&mut clipboard, keyboard, modifiers)
    }

    fn switch_with_backends(
        &self,
        clipboard: &mut impl ClipboardAccess,
        transport: &mut impl SelectionTransport,
        modifiers: ModifierState,
    ) -> Result<SelectedTextSwitchResult, crate::error::SwitcherError> {
        let previous_clipboard = clipboard.get_text().ok();
        let sentinel = unique_clipboard_sentinel();

        clipboard.set_text(&sentinel)?;
        transport.copy_selection(modifiers)?;
        thread::sleep(COPY_SETTLE_DELAY);

        let selected_text = clipboard.get_text()?;
        if selected_text == sentinel {
            restore_clipboard(clipboard, previous_clipboard.as_deref());
            return Ok(SelectedTextSwitchResult::NoSelectedText);
        }

        let Some((converted, direction)) = convert_selected_text(&selected_text) else {
            restore_clipboard(clipboard, previous_clipboard.as_deref());
            return Ok(SelectedTextSwitchResult::NotConvertible);
        };

        clipboard.set_text(&converted)?;
        transport.paste_selection(modifiers)?;
        thread::sleep(PASTE_SETTLE_DELAY);

        let clipboard_restored = restore_clipboard(clipboard, previous_clipboard.as_deref());

        Ok(SelectedTextSwitchResult::Replaced {
            direction,
            clipboard_restored,
        })
    }
}

fn restore_clipboard(clipboard: &mut impl ClipboardAccess, previous: Option<&str>) -> bool {
    match previous {
        Some(text) => clipboard.set_text(text).is_ok(),
        None => {
            let _ = clipboard.set_text("");
            false
        }
    }
}

fn unique_clipboard_sentinel() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("__OPEN_SWITCHER_SELECTION_SENTINEL_{nanos}__")
}

fn convert_selected_text(text: &str) -> Option<(String, ConversionDirection)> {
    let mut converted = String::with_capacity(text.len());
    let mut current_segment_start = 0;
    let mut in_whitespace = None;
    let mut changed_segments = 0usize;
    let mut en_to_ru_segments = 0usize;
    let mut ru_to_en_segments = 0usize;

    for (index, ch) in text.char_indices() {
        let is_whitespace = ch.is_whitespace();

        match in_whitespace {
            None => in_whitespace = Some(is_whitespace),
            Some(kind) if kind != is_whitespace => {
                let segment = &text[current_segment_start..index];
                let segment_kind = kind;
                append_converted_segment(
                    segment,
                    segment_kind,
                    &mut converted,
                    &mut changed_segments,
                    &mut en_to_ru_segments,
                    &mut ru_to_en_segments,
                );
                current_segment_start = index;
                in_whitespace = Some(is_whitespace);
            }
            Some(_) => {}
        }
    }

    if let Some(kind) = in_whitespace {
        let segment = &text[current_segment_start..];
        append_converted_segment(
            segment,
            kind,
            &mut converted,
            &mut changed_segments,
            &mut en_to_ru_segments,
            &mut ru_to_en_segments,
        );
    }

    if changed_segments == 0 || converted == text {
        return None;
    }

    let direction = match (en_to_ru_segments > 0, ru_to_en_segments > 0) {
        (true, true) => ConversionDirection::Mixed,
        (true, false) => ConversionDirection::EnToRu,
        (false, true) => ConversionDirection::RuToEn,
        (false, false) => return None,
    };

    Some((converted, direction))
}

fn append_converted_segment(
    segment: &str,
    is_whitespace: bool,
    output: &mut String,
    changed_segments: &mut usize,
    en_to_ru_segments: &mut usize,
    ru_to_en_segments: &mut usize,
) {
    if is_whitespace {
        output.push_str(segment);
        return;
    }

    if let Some((converted, direction)) = convert_text_segment(segment) {
        output.push_str(&converted);
        *changed_segments += 1;
        match direction {
            ConversionDirection::EnToRu => *en_to_ru_segments += 1,
            ConversionDirection::RuToEn => *ru_to_en_segments += 1,
            ConversionDirection::Mixed => {}
        }
        return;
    }

    output.push_str(segment);
}

fn convert_text_segment(segment: &str) -> Option<(String, ConversionDirection)> {
    let script = dominant_script(segment)?;
    let letter_count = segment_letter_count(segment);
    if letter_count == 0 {
        return None;
    }

    let (direction, converted, current_score, converted_score) = match script {
        Script::Latin => {
            let converted = map_text(segment, en_to_ru_char);
            (
                ConversionDirection::EnToRu,
                converted.clone(),
                score_segment_as_english(segment),
                score_segment_as_russian(&converted),
            )
        }
        Script::Cyrillic => {
            let converted = map_text(segment, ru_to_en_char);
            (
                ConversionDirection::RuToEn,
                converted.clone(),
                score_segment_as_russian(segment),
                score_segment_as_english(&converted),
            )
        }
    };

    if converted == segment {
        return None;
    }

    let min_gain = match letter_count {
        0 | 1 => return None,
        2 => 4,
        _ => 2,
    };

    if converted_score >= current_score + min_gain {
        Some((converted, direction))
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Script {
    Latin,
    Cyrillic,
}

fn dominant_script(segment: &str) -> Option<Script> {
    let mut has_latin = false;
    let mut has_cyrillic = false;

    for ch in segment.chars() {
        if ch.is_ascii_alphabetic() {
            has_latin = true;
        } else if is_cyrillic_letter(ch) {
            has_cyrillic = true;
        }
    }

    match (has_latin, has_cyrillic) {
        (true, false) => Some(Script::Latin),
        (false, true) => Some(Script::Cyrillic),
        _ => None,
    }
}

fn segment_letter_count(segment: &str) -> usize {
    segment
        .chars()
        .filter(|ch| ch.is_ascii_alphabetic() || is_cyrillic_letter(*ch))
        .count()
}

fn score_segment_as_english(segment: &str) -> i32 {
    score_segment(
        segment,
        |ch| ch.is_ascii_alphabetic(),
        is_english_vowel,
        ENGLISH_COMMON_BIGRAMS,
        ENGLISH_AWKWARD_BIGRAMS,
    )
}

fn score_segment_as_russian(segment: &str) -> i32 {
    score_segment(
        segment,
        is_cyrillic_letter,
        is_russian_vowel,
        RUSSIAN_COMMON_BIGRAMS,
        RUSSIAN_AWKWARD_BIGRAMS,
    )
}

fn score_segment(
    segment: &str,
    is_letter: impl Fn(char) -> bool,
    is_vowel: impl Fn(char) -> bool,
    common_bigrams: &[&str],
    awkward_bigrams: &[&str],
) -> i32 {
    let letters: String = segment
        .chars()
        .filter(|ch| is_letter(*ch))
        .flat_map(|ch| ch.to_lowercase())
        .collect();
    if letters.is_empty() {
        return 0;
    }

    let chars: Vec<char> = letters.chars().collect();
    let vowel_count = chars.iter().filter(|ch| is_vowel(**ch)).count();
    let max_consonant_run = max_consonant_run(&chars, &is_vowel);

    let mut score = 0i32;
    score += (vowel_count as i32) * 3;

    if vowel_count == 0 {
        score -= 4;
    } else if vowel_count * 4 >= chars.len() {
        score += 1;
    }

    if max_consonant_run > 3 {
        score -= ((max_consonant_run - 3) as i32) * 3;
    }

    for bigram in common_bigrams {
        if letters.contains(bigram) {
            score += 2;
        }
    }

    for bigram in awkward_bigrams {
        if letters.contains(bigram) {
            score -= 3;
        }
    }

    score
}

fn max_consonant_run(chars: &[char], is_vowel: &impl Fn(char) -> bool) -> usize {
    let mut current = 0usize;
    let mut max_run = 0usize;

    for ch in chars {
        if is_vowel(*ch) {
            current = 0;
            continue;
        }

        current += 1;
        max_run = max_run.max(current);
    }

    max_run
}

fn is_english_vowel(ch: char) -> bool {
    matches!(ch, 'a' | 'e' | 'i' | 'o' | 'u' | 'y')
}

fn is_russian_vowel(ch: char) -> bool {
    matches!(
        ch,
        'а' | 'е' | 'ё' | 'и' | 'о' | 'у' | 'ы' | 'э' | 'ю' | 'я'
    )
}

fn map_text(text: &str, map_char: fn(char) -> char) -> String {
    text.chars().map(map_char).collect()
}

fn is_cyrillic_letter(ch: char) -> bool {
    matches!(ch, 'А'..='Я' | 'а'..='я' | 'Ё' | 'ё')
}

const ENGLISH_COMMON_BIGRAMS: &[&str] = &[
    "th", "he", "in", "er", "an", "re", "on", "at", "en", "nd", "ll", "lo", "or", "rl", "ld", "wo",
];
const ENGLISH_AWKWARD_BIGRAMS: &[&str] = &["qj", "jq", "zx", "xq", "qz", "vh", "hb", "bd", "dt"];
const RUSSIAN_COMMON_BIGRAMS: &[&str] = &[
    "ст", "но", "то", "на", "ен", "ов", "ни", "ра", "ко", "пр", "ве", "ет", "ми", "ир", "ри",
];
const RUSSIAN_AWKWARD_BIGRAMS: &[&str] = &["дщ", "щщ", "ъъ", "ьы", "ыы", "йй", "ьь", "ъы", "ыь"];

fn en_to_ru_char(ch: char) -> char {
    match ch {
        '`' => 'ё',
        '~' => 'Ё',
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
        '\\' => '\\',
        '|' => '/',
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
        ',' => ',',
        '<' => '<',
        '.' => '.',
        '>' => '>',
        '/' => '/',
        '?' => '?',
        '@' => '"',
        '#' => '№',
        '$' => ';',
        '^' => ':',
        '&' => '?',
        _ => ch,
    }
}

fn ru_to_en_char(ch: char) -> char {
    match ch {
        'ё' => '`',
        'Ё' => '~',
        'й' => 'q',
        'Й' => 'Q',
        'ц' => 'w',
        'Ц' => 'W',
        'у' => 'e',
        'У' => 'E',
        'к' => 'r',
        'К' => 'R',
        'е' => 't',
        'Е' => 'T',
        'н' => 'y',
        'Н' => 'Y',
        'г' => 'u',
        'Г' => 'U',
        'ш' => 'i',
        'Ш' => 'I',
        'щ' => 'o',
        'Щ' => 'O',
        'з' => 'p',
        'З' => 'P',
        'х' => '[',
        'Х' => '{',
        'ъ' => ']',
        'Ъ' => '}',
        'ф' => 'a',
        'Ф' => 'A',
        'ы' => 's',
        'Ы' => 'S',
        'в' => 'd',
        'В' => 'D',
        'а' => 'f',
        'А' => 'F',
        'п' => 'g',
        'П' => 'G',
        'р' => 'h',
        'Р' => 'H',
        'о' => 'j',
        'О' => 'J',
        'л' => 'k',
        'Л' => 'K',
        'д' => 'l',
        'Д' => 'L',
        'ж' => ';',
        'Ж' => ':',
        'э' => '\'',
        'Э' => '"',
        'я' => 'z',
        'Я' => 'Z',
        'ч' => 'x',
        'Ч' => 'X',
        'с' => 'c',
        'С' => 'C',
        'м' => 'v',
        'М' => 'V',
        'и' => 'b',
        'И' => 'B',
        'т' => 'n',
        'Т' => 'N',
        'ь' => 'm',
        'Ь' => 'M',
        'б' => ',',
        'Б' => '<',
        'ю' => '.',
        'Ю' => '>',
        '.' => '/',
        ',' => '?',
        '"' => '@',
        '№' => '#',
        ';' => '$',
        ':' => '^',
        '?' => '&',
        _ => ch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_english_layout_to_russian_with_symbols() {
        let converted = convert_selected_text("Ghbdtn, vbh!").unwrap();
        assert_eq!(converted.0, "Привет, мир!");
        assert_eq!(converted.1, ConversionDirection::EnToRu);
    }

    #[test]
    fn converts_russian_layout_to_english_with_symbols() {
        let converted = convert_selected_text("руддщб цщкдв!").unwrap();
        assert_eq!(converted.0, "hello, world!");
        assert_eq!(converted.1, ConversionDirection::RuToEn);
    }

    #[test]
    fn leaves_spaces_and_newlines_while_converting() {
        let converted = convert_selected_text("Ghbdtn,\nVbh!").unwrap();
        assert_eq!(converted.0, "Привет,\nМир!");
    }

    #[test]
    fn reports_not_convertible_for_plain_digits() {
        assert_eq!(convert_selected_text("12345"), None);
    }

    #[test]
    fn converts_only_wrong_layout_segment_in_mixed_phrase() {
        let converted = convert_selected_text("Ghbdtn, мир!").unwrap();
        assert_eq!(converted.0, "Привет, мир!");
        assert_eq!(converted.1, ConversionDirection::EnToRu);
    }

    #[test]
    fn preserves_correct_russian_segment_and_converts_latin_neighbors() {
        let converted = convert_selected_text("Привет, vb hfr?").unwrap();
        assert_eq!(converted.0, "Привет, ми рак?");
        assert_eq!(converted.1, ConversionDirection::EnToRu);
    }

    #[test]
    fn does_not_break_already_correct_english_segment() {
        let converted = convert_selected_text("руддщ? / hello?").unwrap();
        assert_eq!(converted.0, "hello& / hello?");
        assert_eq!(converted.1, ConversionDirection::RuToEn);
    }

    #[test]
    fn leaves_single_character_segments_unchanged() {
        assert_eq!(convert_selected_text("g мир"), None);
    }

    #[test]
    fn keeps_ambiguous_short_segments_unchanged() {
        assert_eq!(convert_selected_text("gh ок"), None);
    }

    #[test]
    fn reports_mixed_direction_when_multiple_segments_change_differently() {
        let converted = convert_selected_text("Ghbdtn hello руддщ").unwrap();
        assert_eq!(converted.0, "Привет hello hello");
        assert_eq!(converted.1, ConversionDirection::Mixed);
    }
}
