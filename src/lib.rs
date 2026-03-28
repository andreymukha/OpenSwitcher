use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use zbus::{dbus_interface, SignalContext};

pub struct SwitcherApi {
    pub enabled: Arc<AtomicBool>,
    pub layout: Arc<AtomicBool>,
}

#[dbus_interface(name = "org.openswitcher.daemon")]
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