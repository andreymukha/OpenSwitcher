use zbus::dbus_interface;
use std::sync::{Arc, atomic::AtomicBool};

pub struct SwitcherApi {
    pub enabled: Arc<AtomicBool>,
}

#[dbus_interface(name = "org.openswitcher.daemon")]
impl SwitcherApi {
    pub fn toggle(&self) {
        use std::sync::atomic::Ordering;
        let old = self.enabled.load(Ordering::SeqCst);
        self.enabled.store(!old, Ordering::SeqCst);
        println!("[API] Смена статуса на: {}", !old);
    }

    #[dbus_interface(property)]
    pub fn is_enabled(&self) -> bool {
        use std::sync::atomic::Ordering;
        self.enabled.load(Ordering::SeqCst)
    }
}
