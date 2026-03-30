use super::dbus_listener::DbusListener;
use async_channel::Sender;
use ksni::{menu::CheckmarkItem, menu::StandardItem, Icon, MenuItem, Tray};

#[derive(Clone, Copy, Debug)]
pub struct TrayState {
    pub enabled: bool,
    pub layout_is_english: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum TrayCommand {
    ShowSettings,
    Quit,
}

pub struct OpenSwitcherTray {
    dbus: DbusListener,
    command_tx: Sender<TrayCommand>,
    pub state: TrayState,
}

impl OpenSwitcherTray {
    pub fn new(dbus: DbusListener, state: TrayState, command_tx: Sender<TrayCommand>) -> Self {
        Self {
            dbus,
            command_tx,
            state,
        }
    }
}

impl Tray for OpenSwitcherTray {
    fn id(&self) -> String {
        "open-switcher".into()
    }

    fn title(&self) -> String {
        "OpenSwitcher".into()
    }

    fn icon_name(&self) -> String {
        String::new()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        draw_icon(self.state.enabled, self.state.layout_is_english)
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            CheckmarkItem {
                label: "Автопереключение".into(),
                checked: self.state.enabled,
                activate: Box::new(|this: &mut OpenSwitcherTray| {
                    if let Err(err) = this.dbus.toggle() {
                        eprintln!("[tray] Failed to toggle OpenSwitcher state: {err}");
                    }
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Настройки...".into(),
                activate: Box::new(|this: &mut OpenSwitcherTray| {
                    if let Err(err) = this.command_tx.try_send(TrayCommand::ShowSettings) {
                        eprintln!("[tray] Failed to request settings window: {err}");
                    }
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Выход".into(),
                activate: Box::new(|this: &mut OpenSwitcherTray| {
                    if let Err(err) = this.command_tx.try_send(TrayCommand::Quit) {
                        eprintln!("[tray] Failed to request tray shutdown: {err}");
                        std::process::exit(1);
                    }
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

fn draw_icon(enabled: bool, layout_is_english: bool) -> Vec<Icon> {
    vec![draw_bitmap_icon(enabled, layout_is_english)]
}

fn draw_bitmap_icon(enabled: bool, layout_is_english: bool) -> Icon {
    let width = 22;
    let height = 22;
    let mut data = vec![0u8; width * height * 4];
    let (bg_r, bg_g, bg_b) = if enabled { (0, 80, 220) } else { (80, 80, 80) };

    for index in 0..(width * height) {
        data[index * 4] = 255;
        data[index * 4 + 1] = bg_r;
        data[index * 4 + 2] = bg_g;
        data[index * 4 + 3] = bg_b;
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

    let (left_mask, right_mask) = if layout_is_english {
        (char_e, char_n)
    } else {
        (char_r, char_u)
    };
    draw_char(&mut data, left_mask, 2, 4, width);
    draw_char(&mut data, right_mask, 12, 4, width);

    Icon {
        width: width as i32,
        height: height as i32,
        data,
    }
}

fn draw_char(data: &mut [u8], mask: &str, x_offset: usize, y_offset: usize, image_width: usize) {
    for (y, line) in mask.trim().lines().enumerate() {
        for (x, ch) in line.trim().chars().enumerate() {
            if ch == '#' {
                let px = (y_offset + y) * image_width + (x_offset + x);
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
