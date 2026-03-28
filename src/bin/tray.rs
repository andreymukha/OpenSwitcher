use ksni::{Tray, MenuItem, TrayService, Status, menu::StandardItem};
use zbus::dbus_proxy;
use zbus::blocking::Connection;
use std::process::Command;
use std::time::Duration;

#[dbus_proxy(
    interface = "org.openswitcher.daemon",
    default_service = "org.openswitcher.daemon",
    default_path = "/org/openswitcher/daemon"
)]
trait Switcher {
    fn toggle(&self) -> zbus::Result<()>;
    #[dbus_proxy(property)]
    fn is_enabled(&self) -> zbus::Result<bool>;
}

struct OpenSwitcherTray {
    proxy: SwitcherProxyBlocking<'static>,
}

impl Tray for OpenSwitcherTray {
    fn id(&self) -> String { "open-switcher-final-v1".into() }
    
    fn icon_name(&self) -> String {
        match self.proxy.is_enabled() {
            Ok(true) => "input-keyboard".into(),
            _ => "dialog-warning".into(),
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let is_on = self.proxy.is_enabled().unwrap_or(true);
        vec![
            StandardItem {
                label: if is_on { "✅ Авто-смена: ВКЛ" } else { "❌ Авто-смена: ВЫКЛ" }.into(),
                activate: Box::new(|this: &mut OpenSwitcherTray| {
                    if let Ok(_) = this.proxy.toggle() {
                        let status = this.proxy.is_enabled().unwrap_or(false);
                        let msg = if status { "ВКЛЮЧЕНА" } else { "ВЫКЛЮЧЕНА" };
                        let _ = Command::new("notify-send")
                            .arg("Open-Switcher")
                            .arg(format!("Авто-смена {}", msg))
                            .arg("-t")
                            .arg("2000")
                            .spawn();
                    }
                }),
                ..Default::default()
            }.into(),
            MenuItem::Separator,
            StandardItem {
                label: "Выход".into(),
                activate: Box::new(|_| std::process::exit(0)),
                ..Default::default()
            }.into(),
        ]
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let connection = Connection::session()?;
    let proxy = SwitcherProxyBlocking::new(&connection)?;
    let tray = OpenSwitcherTray { proxy };
    let service = TrayService::new(tray);
    let handle = service.handle();
    service.spawn();
    loop {
        std::thread::sleep(Duration::from_millis(1000));
        handle.update(|_| {});
    }
}
