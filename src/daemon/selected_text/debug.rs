use crate::daemon::debug_log::{format_selected, try_debug_line, DebugLogKind};

pub(crate) fn log_selected_text_debug(stage: &str, details: &str) {
    let _ = try_debug_line(DebugLogKind::SelectedText, || {
        format_selected(stage, details)
    });
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

#[cfg(test)]
mod tests {
    use super::summarize_text;
    use crate::daemon::debug_log::format_selected;

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

    #[test]
    fn formatted_selected_text_debug_line_contains_only_the_summary() {
        let input = "password=secret-token";

        let line = format_selected("copy", &summarize_text(input));

        assert!(!line.contains("password"));
        assert!(!line.contains("secret"));
        assert!(!line.contains("token"));
        assert!(line.contains("chars="));
        assert!(line.starts_with("[selected-text-debug] stage=copy "));
    }
}
