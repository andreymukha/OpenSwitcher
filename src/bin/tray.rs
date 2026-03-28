use ksni::{menu::CheckmarkItem, menu::StandardItem, Icon, MenuItem, Tray, TrayService};
use std::process::Command;
use std::sync::mpsc;
use zbus::blocking::Connection;
use zbus::dbus_proxy;

#[dbus_proxy(
    interface = "org.oswitch.core",
    default_service = "org.oswitch.core",
    default_path = "/org/oswitch/core"
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

fn draw_icon(enabled: bool, is_en: bool) -> Vec<Icon> {
    let width = 22;
    let height = 22;
    let mut data = vec![0u8; (width * height * 4) as usize];

    // Синий (активный), Серый (неактивный)
    let (bg_a, bg_r, bg_g, bg_b) = if enabled {
        (255, 30, 144, 255) // Синий
    } else {
        (255, 120, 120, 120) // Серый
    };

    // Заливаем фон
    for i in 0..(width * height) as usize {
        data[i * 4] = bg_a;
        data[i * 4 + 1] = bg_r;
        data[i * 4 + 2] = bg_g;
        data[i * 4 + 3] = bg_b;
    }

    let char_e = [
        [1, 1, 1, 1, 1],
        [1, 1, 0, 0, 0],
        [1, 1, 0, 0, 0],
        [1, 1, 1, 1, 0],
        [1, 1, 0, 0, 0],
        [1, 1, 0, 0, 0],
        [1, 1, 1, 1, 1],
    ];
    let char_n = [
        [1, 1, 0, 0, 1],
        [1, 1, 1, 0, 1],
        [1, 1, 1, 0, 1],
        [1, 1, 1, 1, 1],
        [1, 0, 1, 1, 1],
        [1, 0, 1, 1, 1],
        [1, 0, 0, 1, 1],
    ];
    let char_r = [
        [1, 1, 1, 1, 0],
        [1, 1, 0, 1, 1],
        [1, 1, 0, 1, 1],
        [1, 1, 1, 1, 0],
        [1, 1, 0, 1, 1],
        [1, 1, 0, 1, 1],
        [1, 1, 0, 1, 1],
    ];
    let char_u = [
        [1, 1, 0, 1, 1],
        [1, 1, 0, 1, 1],
        [1, 1, 0, 1, 1],
        [1, 1, 0, 1, 1],
        [1, 1, 0, 1, 1],
        [1, 1, 0, 1, 1],
        [0, 1, 1, 1, 0],
    ];

    let (left_char, right_char) = if is_en {
        (char_e, char_n)
    } else {
        (char_r, char_u)
    };

    let start_y = 7;
    let start_x_left = 5;
    let start_x_right = 12;

    // Рисуем буквы (белым цветом)
    for y in 0..7 {
        for x in 0..5 {
            if left_char[y][x] == 1 {
                let px = (start_y + y) * width + (start_x_left + x);
                let idx = (px * 4) as usize;
                data[idx] = 255;
                data[idx + 1] = 255;
                data[idx + 2] = 255;
                data[idx + 3] = 255;
            }
            if right_char[y][x] == 1 {
                let px = (start_y + y) * width + (start_x_right + x);
                let idx = (px * 4) as usize;
                data[idx] = 255;
                data[idx + 1] = 255;
                data[idx + 2] = 255;
                data[idx + 3] = 255;
            }
        }
    }

    vec![Icon {
        width: width as i32,
        height: height as i32,
        data,
    }]
}

impl Tray for OpenSwitcherTray {
    fn id(&self) -> String {
        "open-switcher-final-v6".into()
    }

    fn title(&self) -> String {
        "OpenSwitcher".into()
    }

    fn icon_name(&self) -> String {
        "".into()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        draw_icon(self.enabled, self.layout)
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            CheckmarkItem {
                label: "Автопереключение".into(),
                checked: self.enabled,
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
                label: "Настройки...".into(),
                activate: Box::new(|_| {
                    std::thread::spawn(|| {
                        let config_path = open_switcher::get_config_path();
                        let script_path = format!("{}/projects/open-switcher/settings_ui.py", std::env::var("HOME").unwrap_or("/home/fly".into()));
                        
                        let _ = Command::new("python3")
                            .arg(&script_path)
                            .arg(&config_path)
                            .spawn();
                    });
                }),
                ..Default::default()
            }.into(),
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
                    eprintln!("signal error: {}", err);
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
