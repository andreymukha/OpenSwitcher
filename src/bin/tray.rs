use ksni::{menu::CheckmarkItem, menu::StandardItem, Icon, MenuItem, Tray, TrayService};
use std::process::Command;
use std::sync::mpsc;
use zbus::blocking::Connection;
use zbus::dbus_proxy;
use std::time::Duration;

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

fn draw_char(data: &mut [u8], mask: &str, x_offset: usize, y_offset: usize, img_w: usize) {
    for (y, line) in mask.trim().lines().enumerate() {
        for (x, c) in line.trim().chars().enumerate() {
            if c == '#' {
                let px = (y_offset + y) * img_w + (x_offset + x);
                let idx = px * 4;
                if idx + 3 < data.len() {
                    data[idx] = 255;
                    data[idx + 1] = 255;
                    data[idx + 2] = 255;
                    data[idx + 3] = 255;
                }
            }
        }
    }
}

fn draw_icon(enabled: bool, is_en: bool) -> Vec<Icon> {
    let w = 22;
    let h = 22;
    let mut data = vec![0u8; w * h * 4];
    let (bg_r, bg_g, bg_b) = if enabled { (0, 80, 220) } else { (80, 80, 80) };

    for i in 0..(w * h) {
        data[i * 4] = 255;
        data[i * 4 + 1] = bg_r;
        data[i * 4 + 2] = bg_g;
        data[i * 4 + 3] = bg_b;
    }

    let char_e = "
        #########
        #########
        ###
        ###
        #######
        #######
        ###
        ###
        #########
        #########
    ";
    let char_n = "
        ###   ###
        ####  ###
        ##### ###
        ### #####
        ###  ####
        ###   ###
        ###   ###
        ###   ###
        ###   ###
        ###   ###
    ";
    let char_r = "
        ########
        #########
        ###   ###
        ###   ###
        #########
        ########
        ### ###
        ###  ###
        ###   ###
        ###   ###
    ";
    let char_u = "
        ###   ###
        ###   ###
        ###   ###
        ###   ###
        ###   ###
        ###   ###
        ###   ###
        #########
         #######
          #####
    ";

    let (l_mask, r_mask) = if is_en { (char_e, char_n) } else { (char_r, char_u) };
    draw_char(&mut data, l_mask, 2, 4, w);
    draw_char(&mut data, r_mask, 12, 4, w);

    vec![Icon { width: w as i32, height: h as i32, data }]
}

impl Tray for OpenSwitcherTray {
    fn id(&self) -> String { "open-switcher-final-v7".into() }
    fn title(&self) -> String { "OpenSwitcher".into() }
    fn icon_name(&self) -> String { "".into() }
    fn icon_pixmap(&self) -> Vec<Icon> { draw_icon(self.enabled, self.layout) }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            CheckmarkItem {
                label: "Автопереключение".into(),
                checked: self.enabled,
                activate: Box::new(|this: &mut OpenSwitcherTray| {
                    if this.proxy.toggle().is_ok() {
                        let new_state = !this.enabled;
                        let _ = this.tx_update.send((new_state, this.layout));
                    }
                }),
                ..Default::default()
            }.into(),
            MenuItem::Separator,
            StandardItem {
                label: "Настройки...".into(),
                activate: Box::new(|_| {
                    std::thread::spawn(|| {
                        let config_path = open_switcher::get_config_path();
                        let script_path = open_switcher::get_config_dir().join("settings_ui.py");
                        let _ = Command::new("python3").arg(&script_path).arg(config_path.to_str().unwrap()).spawn();
                    });
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
    let initial_enabled = proxy.is_enabled().unwrap_or(true);
    let initial_layout = proxy.current_layout().unwrap_or(true);
    let (tx, rx) = mpsc::channel();

    let tray = OpenSwitcherTray { proxy, enabled: initial_enabled, layout: initial_layout, tx_update: tx.clone() };
    let service = TrayService::new(tray);
    let handle = service.handle();
    service.spawn();

    let signal_conn = Connection::session()?;
    let signal_proxy = SwitcherProxyBlocking::new(&signal_conn)?;
    let tx_signal = tx.clone();

    std::thread::spawn(move || {
        loop {
            if let Ok(mut stream) = signal_proxy.receive_status_changed() {
                for signal in &mut stream {
                    if let Ok(args) = signal.args() {
                        let _ = tx_signal.send((args.enabled, args.layout));
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(500));
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
