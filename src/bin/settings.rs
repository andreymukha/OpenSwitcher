#[cfg(feature = "settings-ui")]
fn main() {
    open_switcher::settings_ui::run();
}

#[cfg(not(feature = "settings-ui"))]
fn main() {
    unreachable!("open-switcher-settings requires the settings-ui feature");
}
