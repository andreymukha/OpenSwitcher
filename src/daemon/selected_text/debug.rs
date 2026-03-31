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
    const LIMIT: usize = 80;
    let sanitized = text.replace('\n', "\\n");
    if sanitized.chars().count() <= LIMIT {
        return format!("{sanitized:?}");
    }

    let prefix: String = sanitized.chars().take(LIMIT).collect();
    format!("{prefix:?}...")
}

fn append_selected_text_debug_line(line: &str) {
    let path = std::env::var(SELECTED_TEXT_DEBUG_FILE_ENV)
        .unwrap_or_else(|_| "/tmp/open-switcher-selected-text.log".to_string());

    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}
