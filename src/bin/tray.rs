#[cfg(feature = "settings-ui")]
fn main() -> Result<(), open_switcher::error::SwitcherError> {
    open_switcher::tray::run()
}

#[cfg(not(feature = "settings-ui"))]
fn main() {
    unreachable!("open-switcher-tray requires the settings-ui feature");
}
