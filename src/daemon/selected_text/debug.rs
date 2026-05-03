use std::fs::OpenOptions;
use std::io::Write;
use std::sync::OnceLock;

const SELECTED_TEXT_DEBUG_ENV: &str = "OPEN_SWITCHER_SELECTED_TEXT_DEBUG";
const SELECTED_TEXT_DEBUG_FILE_ENV: &str = "OPEN_SWITCHER_SELECTED_TEXT_DEBUG_FILE";

pub(crate) fn selected_text_debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var(SELECTED_TEXT_DEBUG_ENV)
            .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "on"))
            .unwrap_or(false)
    })
}

pub(crate) fn log_selected_text_debug(stage: &str, details: &str) {
    if !selected_text_debug_enabled() {
        return;
    }

    let line = format!("[selected-text-debug] stage={stage} {details}");
    eprintln!("{line}");
    append_selected_text_debug_line(&line);
}

pub(crate) fn summarize_text(text: &str) -> String {
    let chars = text.chars().count();
    let bytes = text.len();
    let lines = if text.is_empty() {
        0
    } else {
        text.split('\n').count()
    };
    let empty = text.is_empty();
    let has_newline = text.contains('\n');

    format!("chars={chars} bytes={bytes} lines={lines} empty={empty} has_newline={has_newline}")
}

fn append_selected_text_debug_line(line: &str) {
    let path = std::env::var(SELECTED_TEXT_DEBUG_FILE_ENV)
        .unwrap_or_else(|_| "/tmp/open-switcher-selected-text.log".to_string());

    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::summarize_text;

    #[test]
    fn summarize_text_redacts_content() {
        let input = "password=secret-token";
        let summary = summarize_text(input);

        assert!(!summary.contains("password"));
        assert!(!summary.contains("secret"));
        assert!(!summary.contains("token"));
        assert!(!summary.contains(input));
        assert!(summary.contains("chars="));
        assert!(summary.contains("bytes="));
        assert!(summary.contains("lines="));
        assert!(summary.contains("empty=false"));
    }

    #[test]
    fn summarize_text_reports_empty_and_multiline_metadata() {
        let empty_summary = summarize_text("");

        assert!(empty_summary.contains("chars=0"));
        assert!(empty_summary.contains("bytes=0"));
        assert!(empty_summary.contains("lines=0"));
        assert!(empty_summary.contains("empty=true"));

        let multiline = "first private line\nsecond private line";
        let multiline_summary = summarize_text(multiline);

        assert!(multiline_summary.contains("has_newline=true"));
        assert!(multiline_summary.contains("lines=2"));
        assert!(!multiline_summary.contains("first private line"));
        assert!(!multiline_summary.contains("second private line"));
    }
}
