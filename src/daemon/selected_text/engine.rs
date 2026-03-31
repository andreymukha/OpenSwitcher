#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversionDirection {
    EnToRu,
    RuToEn,
    Mixed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversionOutcome {
    pub converted_text: String,
    pub direction: ConversionDirection,
}

#[derive(Default)]
pub(super) struct LayoutConversionEngine;

impl LayoutConversionEngine {
    pub(super) fn convert_selected_text(&self, text: &str) -> Option<ConversionOutcome> {
        let segments = segment_text(text);
        let mut converted = String::with_capacity(text.len());
        let mut en_to_ru_segments = 0usize;
        let mut ru_to_en_segments = 0usize;
        let mut changed_segments = 0usize;

        for segment in &segments {
            match segment.kind {
                SegmentKind::Whitespace | SegmentKind::Separator => {
                    converted.push_str(segment.text);
                }
                SegmentKind::Text => match decide_segment_conversion(segment.text) {
                    SegmentDecision::Keep => converted.push_str(segment.text),
                    SegmentDecision::Convert {
                        direction,
                        converted_text,
                    } => {
                        converted.push_str(&converted_text);
                        changed_segments += 1;
                        match direction {
                            ConversionDirection::EnToRu => en_to_ru_segments += 1,
                            ConversionDirection::RuToEn => ru_to_en_segments += 1,
                            ConversionDirection::Mixed => {}
                        }
                    }
                },
            }
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

        Some(ConversionOutcome {
            converted_text: converted,
            direction,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SegmentKind {
    Text,
    Whitespace,
    Separator,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Segment<'a> {
    pub kind: SegmentKind,
    pub text: &'a str,
}

pub(super) fn segment_text(text: &str) -> Vec<Segment<'_>> {
    let mut segments = Vec::new();
    let mut current_start = 0usize;
    let mut current_kind = None;

    for (index, ch) in text.char_indices() {
        let kind = classify_char(ch);
        match current_kind {
            None => current_kind = Some(kind),
            Some(existing) if existing == kind => {}
            Some(existing) => {
                segments.push(Segment {
                    kind: existing,
                    text: &text[current_start..index],
                });
                current_start = index;
                current_kind = Some(kind);
            }
        }
    }

    if let Some(kind) = current_kind {
        segments.push(Segment {
            kind,
            text: &text[current_start..],
        });
    }

    segments
}

fn classify_char(ch: char) -> SegmentKind {
    if ch.is_whitespace() {
        SegmentKind::Whitespace
    } else if is_segment_text_char(ch) {
        SegmentKind::Text
    } else {
        SegmentKind::Separator
    }
}

fn is_segment_text_char(ch: char) -> bool {
    ch.is_ascii_alphabetic() || is_cyrillic_letter(ch)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Script {
    Latin,
    Cyrillic,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SegmentAnalysis<'a> {
    original: &'a str,
    dominant_script: Option<Script>,
    letter_count: usize,
    english_score: i32,
    russian_score: i32,
}

fn analyze_segment(segment: &str) -> SegmentAnalysis<'_> {
    SegmentAnalysis {
        original: segment,
        dominant_script: dominant_script(segment),
        letter_count: segment_letter_count(segment),
        english_score: score_segment_as_english(segment),
        russian_score: score_segment_as_russian(segment),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SegmentDecision {
    Keep,
    Convert {
        direction: ConversionDirection,
        converted_text: String,
    },
}

fn decide_segment_conversion(segment: &str) -> SegmentDecision {
    let analysis = analyze_segment(segment);
    let Some(script) = analysis.dominant_script else {
        return SegmentDecision::Keep;
    };

    if analysis.letter_count <= 1 {
        return SegmentDecision::Keep;
    }

    let (direction, converted, current_score, converted_score) = match script {
        Script::Latin => {
            let converted = map_text(segment, en_to_ru_char);
            (
                ConversionDirection::EnToRu,
                converted.clone(),
                analysis.english_score,
                score_segment_as_russian(&converted),
            )
        }
        Script::Cyrillic => {
            let converted = map_text(segment, ru_to_en_char);
            (
                ConversionDirection::RuToEn,
                converted.clone(),
                analysis.russian_score,
                score_segment_as_english(&converted),
            )
        }
    };

    if converted == segment {
        return SegmentDecision::Keep;
    }

    let min_gain = match analysis.letter_count {
        0 | 1 => unreachable!("short segments are filtered out above"),
        2 => 4,
        _ => 2,
    };

    if converted_score >= current_score + min_gain {
        SegmentDecision::Convert {
            direction,
            converted_text: converted,
        }
    } else {
        SegmentDecision::Keep
    }
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
        .filter(|ch| is_segment_text_char(*ch))
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
    use std::borrow::Cow;

    fn convert(text: &str) -> Option<ConversionOutcome> {
        LayoutConversionEngine.convert_selected_text(text)
    }

    #[test]
    fn splits_text_into_explicit_segment_kinds() {
        let segments = segment_text("Ghbdtn, мир!\nhello");
        let simplified: Vec<(SegmentKind, Cow<'_, str>)> = segments
            .iter()
            .map(|segment| (segment.kind, Cow::from(segment.text)))
            .collect();
        assert_eq!(
            simplified,
            vec![
                (SegmentKind::Text, Cow::from("Ghbdtn")),
                (SegmentKind::Separator, Cow::from(",")),
                (SegmentKind::Whitespace, Cow::from(" ")),
                (SegmentKind::Text, Cow::from("мир")),
                (SegmentKind::Separator, Cow::from("!")),
                (SegmentKind::Whitespace, Cow::from("\n")),
                (SegmentKind::Text, Cow::from("hello")),
            ]
        );
    }

    #[test]
    fn converts_english_layout_to_russian_with_symbols() {
        let converted = convert("Ghbdtn, vbh!").unwrap();
        assert_eq!(converted.converted_text, "Привет, мир!");
        assert_eq!(converted.direction, ConversionDirection::EnToRu);
    }

    #[test]
    fn converts_russian_layout_to_english_with_symbols() {
        let converted = convert("руддщб цщкдв!").unwrap();
        assert_eq!(converted.converted_text, "hello, world!");
        assert_eq!(converted.direction, ConversionDirection::RuToEn);
    }

    #[test]
    fn leaves_spaces_and_newlines_while_converting() {
        let converted = convert("Ghbdtn,\nVbh!").unwrap();
        assert_eq!(converted.converted_text, "Привет,\nМир!");
    }

    #[test]
    fn reports_not_convertible_for_plain_digits() {
        assert_eq!(convert("12345"), None);
    }

    #[test]
    fn converts_only_wrong_layout_segment_in_mixed_phrase() {
        let converted = convert("Ghbdtn, мир!").unwrap();
        assert_eq!(converted.converted_text, "Привет, мир!");
        assert_eq!(converted.direction, ConversionDirection::EnToRu);
    }

    #[test]
    fn preserves_correct_russian_segment_and_converts_latin_neighbors() {
        let converted = convert("Привет, vb hfr?").unwrap();
        assert_eq!(converted.converted_text, "Привет, ми рак?");
        assert_eq!(converted.direction, ConversionDirection::EnToRu);
    }

    #[test]
    fn does_not_break_already_correct_english_segment() {
        let converted = convert("руддщ? / hello?").unwrap();
        assert_eq!(converted.converted_text, "hello? / hello?");
        assert_eq!(converted.direction, ConversionDirection::RuToEn);
    }

    #[test]
    fn leaves_single_character_segments_unchanged() {
        assert_eq!(convert("g мир"), None);
    }

    #[test]
    fn keeps_ambiguous_short_segments_unchanged() {
        assert_eq!(convert("gh ок"), None);
    }

    #[test]
    fn reports_mixed_direction_when_multiple_segments_change_differently() {
        let converted = convert("Ghbdtn hello руддщ").unwrap();
        assert_eq!(converted.converted_text, "Привет hello hello");
        assert_eq!(converted.direction, ConversionDirection::Mixed);
    }

    #[test]
    fn leaves_separator_only_fragments_unchanged() {
        assert_eq!(convert("... / !!!"), None);
    }
}
