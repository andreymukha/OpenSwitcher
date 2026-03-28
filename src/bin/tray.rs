use ksni::{menu::StandardItem, MenuItem, Tray, TrayService};
use std::process::Command;
use std::sync::mpsc;
use zbus::blocking::Connection;
use zbus::dbus_proxy;

#[dbus_proxy(
    interface = "org.openswitcher.daemon",
    default_service = "org.openswitcher.daemon",
    default_path = "/org/openswitcher/daemon"
)]
trait Switcher {
    fn toggle(&self) -> zbus::Result<()>;

    #[dbus_proxy(property)]
    fn is_enabled(&self) -> zbus::Result<bool>;

    #[dbus_proxy(property)]
    fn current_layout(&self) -> zbus::Result<bool>;

    #[dbus_proxy(signal)]
    fn status_changed(&self, enabled: bool, layout: bool) -> zbus::Result<()>;
}

struct OpenSwitcherTray {
    proxy: SwitcherProxyBlocking<'static>,
    enabled: bool,
    layout: bool,
    tx_update: mpsc::Sender<(bool, bool)>,
}

impl Tray for OpenSwitcherTray {
    fn id(&self) -> String {
        "open-switcher-final-v4".into()
    }

    fn title(&self) -> String {
        let layout = if self.layout { "EN" } else { "RU" };
        let status = if self.enabled { "ВКЛ" } else { "ВЫКЛ" };
        format!("OpenSwitcher: {} [{}]", status, layout)
    }

    fn icon_name(&self) -> String {
        if self.layout {
            "input-keyboard".into()
        } else {
            "preferences-desktop-locale".into()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label: if self.enabled {
                    "✅ Авто-смена: ВКЛ".into()
                } else {
                    "❌ Авто-смена: ВЫКЛ".into()
                },
                activate: Box::new(|this: &mut OpenSwitcherTray| {
                    if this.proxy.toggle().is_ok() {
                        let new_state = !this.enabled;
                        let _ = this.tx_update.send((new_state, this.layout));

                        let msg = if new_state { "ВКЛЮЧЕНА" } else { "ВЫКЛЮЧЕНА" };
                        let _ = Command::new("notify-send")
                            .arg("Open-Switcher")
                            .arg(format!("Авто-смена {}", msg))
                            .arg("-t")
                            .arg("2000")
                            .spawn();
                    }
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Выход".into(),
                activate: Box::new(|_| std::process::exit(0)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let connection = Connection::session()?;
    let proxy = SwitcherProxyBlocking::new(&connection)?;

    let initial_enabled = proxy.is_enabled().unwrap_or(true);
    let initial_layout = proxy.current_layout().unwrap_or(true);

    let (tx, rx) = mpsc::channel();

    let tray = OpenSwitcherTray {
        proxy,
        enabled: initial_enabled,
        layout: initial_layout,
        tx_update: tx.clone(),
    };

    let service = TrayService::new(tray);
    let handle = service.handle();
    service.spawn();

    let signal_conn = Connection::session()?;
    let signal_proxy = SwitcherProxyBlocking::new(&signal_conn)?;
    let tx_signal = tx.clone();

    std::thread::spawn(move || {
        loop {
            match signal_proxy.receive_status_changed() {
                Ok(mut stream) => {
                    for signal in &mut stream {
                        if let Ok(args) = signal.args() {
                            let _ = tx_signal.send((args.enabled, args.layout));
                        }
                    }
                }
                Err(err) => {
                    eprintln!("signal error: {err}");
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            }
        }
    });

    for (enabled, layout) in rx {
        handle.update(|tray| {
            tray.enabled = enabled;
            tray.layout = layout;
        });
    }

    Ok(())
}