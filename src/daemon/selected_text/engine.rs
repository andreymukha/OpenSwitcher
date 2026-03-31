use super::{log_selected_text_debug, summarize_text};

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
    pub(super) fn convert_selected_text(&self, text: &str) -> ConversionOutcome {
        let segments = segment_text(text);
        log_selected_text_debug(
            "segment-start",
            &format!("input={}", summarize_text(text)),
        );
        log_selected_text_debug(
            "segment-list",
            &format!("segments={}", summarize_segments(&segments)),
        );
        let mut converted = String::with_capacity(text.len());
        let mut en_to_ru_segments = 0usize;
        let mut ru_to_en_segments = 0usize;
        let mut text_segments = 0usize;
        let mut last_text_direction: Option<ConversionDirection> = None;

        for (index, segment) in segments.iter().enumerate() {
            match segment.kind {
                SegmentKind::Whitespace | SegmentKind::Separator => {
                    let separator_direction =
                        last_text_direction.or_else(|| next_text_direction(&segments, index + 1));

                    if segment.kind == SegmentKind::Separator {
                        if let Some(direction) = separator_direction {
                            let converted_separator = convert_with_direction(segment.text, direction);
                            log_selected_text_debug(
                                "separator-conversion",
                                &format!(
                                    "segment={} direction={direction:?} converted_preview={}",
                                    summarize_text(segment.text),
                                    summarize_text(&converted_separator)
                                ),
                            );
                            converted.push_str(&converted_separator);
                        } else {
                            converted.push_str(segment.text);
                        }
                    } else {
                        converted.push_str(segment.text);
                    }
                }
                SegmentKind::Text => {
                    text_segments += 1;
                    let evaluation = evaluate_segment_conversion(segment.text);
                    log_selected_text_debug(
                        "segment-evaluation",
                        &format!(
                            "segment={} latin_letters={} cyrillic_letters={} direction={:?} reason={} converted_preview={}",
                            summarize_text(segment.text),
                            evaluation.latin_letter_count,
                            evaluation.cyrillic_letter_count,
                            evaluation.direction,
                            evaluation.reason,
                            summarize_text(&evaluation.converted_text)
                        ),
                    );

                    converted.push_str(&evaluation.converted_text);
                    last_text_direction = Some(evaluation.direction);
                    match evaluation.direction {
                        ConversionDirection::EnToRu => en_to_ru_segments += 1,
                        ConversionDirection::RuToEn => ru_to_en_segments += 1,
                        ConversionDirection::Mixed => {}
                    }
                }
            }
        }

        let direction = match (en_to_ru_segments > 0, ru_to_en_segments > 0) {
            (true, true) => ConversionDirection::Mixed,
            (true, false) => ConversionDirection::EnToRu,
            (false, true) => ConversionDirection::RuToEn,
            (false, false) => ConversionDirection::EnToRu,
        };

        log_selected_text_debug(
            "final-decision",
            &format!(
                "result=Replaced direction={direction:?} text_segments={text_segments} output={}",
                summarize_text(&converted)
            ),
        );

        ConversionOutcome {
            converted_text: converted,
            direction,
        }
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct SegmentEvaluation {
    latin_letter_count: usize,
    cyrillic_letter_count: usize,
    direction: ConversionDirection,
    converted_text: String,
    reason: &'static str,
}

fn evaluate_segment_conversion(segment: &str) -> SegmentEvaluation {
    let latin_letter_count = segment.chars().filter(|ch| ch.is_ascii_alphabetic()).count();
    let cyrillic_letter_count = segment.chars().filter(|ch| is_cyrillic_letter(*ch)).count();

    let (direction, reason) = if cyrillic_letter_count > latin_letter_count {
        (ConversionDirection::RuToEn, "majority-cyrillic")
    } else if latin_letter_count > cyrillic_letter_count {
        (ConversionDirection::EnToRu, "majority-latin")
    } else {
        (ConversionDirection::EnToRu, "default-en-to-ru")
    };

    let converted_text = convert_with_direction(segment, direction);

    SegmentEvaluation {
        latin_letter_count,
        cyrillic_letter_count,
        direction,
        converted_text,
        reason,
    }
}

fn summarize_segments(segments: &[Segment<'_>]) -> String {
    segments
        .iter()
        .map(|segment| format!("{:?}:{}", segment.kind, summarize_text(segment.text)))
        .collect::<Vec<_>>()
        .join(" | ")
}

fn next_text_direction(segments: &[Segment<'_>], start_index: usize) -> Option<ConversionDirection> {
    for segment in &segments[start_index..] {
        if segment.kind == SegmentKind::Text {
            return Some(evaluate_segment_conversion(segment.text).direction);
        }
    }

    None
}

fn convert_with_direction(text: &str, direction: ConversionDirection) -> String {
    match direction {
        ConversionDirection::EnToRu | ConversionDirection::Mixed => map_text(text, en_to_ru_char),
        ConversionDirection::RuToEn => map_text(text, ru_to_en_char),
    }
}

fn map_text(text: &str, map_char: fn(char) -> char) -> String {
    text.chars().map(map_char).collect()
}

fn is_cyrillic_letter(ch: char) -> bool {
    matches!(ch, 'А'..='Я' | 'а'..='я' | 'Ё' | 'ё')
}

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
        '<' => 'Б',
        '.' => '.',
        '>' => 'Ю',
        '/' => '.',
        '?' => ',',
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

    fn convert(text: &str) -> ConversionOutcome {
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
        let converted = convert("Ghbdtn, vbh!");
        assert_eq!(converted.converted_text, "Привет, мир!");
        assert_eq!(converted.direction, ConversionDirection::EnToRu);
    }

    #[test]
    fn converts_russian_layout_to_english_with_symbols() {
        let converted = convert("руддщб цщкдв!");
        assert_eq!(converted.converted_text, "hello, world!");
        assert_eq!(converted.direction, ConversionDirection::RuToEn);
    }

    #[test]
    fn leaves_spaces_and_newlines_while_converting() {
        let converted = convert("Ghbdtn,\nVbh!");
        assert_eq!(converted.converted_text, "Привет,\nМир!");
    }

    #[test]
    fn leaves_plain_digits_unchanged() {
        let converted = convert("12345");
        assert_eq!(converted.converted_text, "12345");
        assert_eq!(converted.direction, ConversionDirection::EnToRu);
    }

    #[test]
    fn converts_only_wrong_layout_segment_in_mixed_phrase() {
        let converted = convert("Ghbdtn, мир!");
        assert_eq!(converted.converted_text, "Привет, vbh!");
        assert_eq!(converted.direction, ConversionDirection::Mixed);
    }

    #[test]
    fn always_converts_each_text_segment_in_mixed_phrase() {
        let converted = convert("Привет, vb hfr?");
        assert_eq!(converted.converted_text, "Ghbdtn? ми рак,");
        assert_eq!(converted.direction, ConversionDirection::Mixed);
    }

    #[test]
    fn converts_both_sides_of_bidirectional_mixed_text() {
        let converted = convert("руддщ? / hello?");
        assert_eq!(converted.converted_text, "hello& / руддщ,");
        assert_eq!(converted.direction, ConversionDirection::Mixed);
    }

    #[test]
    fn converts_single_character_segments_too() {
        let converted = convert("g мир");
        assert_eq!(converted.converted_text, "п vbh");
        assert_eq!(converted.direction, ConversionDirection::Mixed);
    }

    #[test]
    fn converts_ambiguous_short_segments_instead_of_rejecting() {
        let converted = convert("gh ок");
        assert_eq!(converted.converted_text, "пр jr");
        assert_eq!(converted.direction, ConversionDirection::Mixed);
    }

    #[test]
    fn reports_mixed_direction_when_multiple_segments_change_differently() {
        let converted = convert("Ghbdtn hello руддщ");
        assert_eq!(converted.converted_text, "Привет руддщ hello");
        assert_eq!(converted.direction, ConversionDirection::Mixed);
    }

    #[test]
    fn leaves_separator_only_fragments_stable() {
        let converted = convert("... / !!!");
        assert_eq!(converted.converted_text, "... / !!!");
        assert_eq!(converted.direction, ConversionDirection::EnToRu);
    }

    #[test]
    fn converts_already_correct_english_text_when_user_explicitly_requests_it() {
        let converted = convert("hello world");
        assert_eq!(converted.converted_text, "руддщ цщкдв");
        assert_eq!(converted.direction, ConversionDirection::EnToRu);
    }

    #[test]
    fn converts_punctuation_next_to_text_segments() {
        let converted = convert("ghbdtn? vbh/");
        assert_eq!(converted.converted_text, "привет, мир.");
        assert_eq!(converted.direction, ConversionDirection::EnToRu);
    }
}
