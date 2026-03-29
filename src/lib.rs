use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use zbus::{dbus_interface, SignalContext};
use std::path::PathBuf;
use std::fs;

pub fn get_config_dir() -> PathBuf {
    let mut path = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()));
    path.push(".config");
    path.push("open-switcher");
    let _ = fs::create_dir_all(&path);
    path
}

pub fn get_config_path() -> PathBuf {
    let mut path = get_config_dir();
    path.push("config.toml");
    
    if !path.exists() {
        let default_config = r#"[layout]
keys = ["LeftControl", "LeftShift"]
delay_ms = 30

[delays]
backspace_ms = 0
typing_ms = 0

[features]
undo_key = "Pause"
"#;
        let _ = fs::write(&path, default_config);
    }
    
    path
}

pub struct SwitcherApi {
    pub enabled: Arc<AtomicBool>,
    pub layout: Arc<AtomicBool>,
}

#[dbus_interface(name = "org.oswitch.core")]
impl SwitcherApi {
    pub fn toggle(&self, #[zbus(signal_context)] ctxt: SignalContext<'_>) {
        let new_enabled = !self.enabled.load(Ordering::SeqCst);
        self.enabled.store(new_enabled, Ordering::SeqCst);

        println!("[API] enabled: {}", new_enabled);

        let layout = self.layout.load(Ordering::SeqCst);

        let _ = zbus::block_on(Self::status_changed(&ctxt, new_enabled, layout));
    }

    #[dbus_interface(property)]
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    #[dbus_interface(property)]
    pub fn current_layout(&self) -> bool {
        self.layout.load(Ordering::SeqCst)
    }

    #[dbus_interface(signal)]
    pub async fn status_changed(
        ctxt: &SignalContext<'_>,
        enabled: bool,
        layout: bool,
    ) -> zbus::Result<()>;
}
