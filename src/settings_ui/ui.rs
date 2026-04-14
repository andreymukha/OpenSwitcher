use super::dbus_client::SettingsDbusClient;
use super::presenter::{PresenterEvent, SaveRequest, SettingsPresenter};
use super::state::{LayoutSwitchActionsState, LayoutSwitchViewState, ViewState};
use crate::error::SettingsClientError;
use crate::model::{
    LayoutSwitchCapturePhase, LayoutSwitchCaptureState, LayoutSwitchCombo, SelectedTextHotkey,
};
use adw::prelude::*;
use gtk::gdk;
use gtk::glib::{self, SignalHandlerId};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

const WINDOW_WIDTH: i32 = 760;
const WINDOW_HEIGHT: i32 = 520;
const PAGE_MAX_WIDTH: i32 = 560;
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(60);
const CAPTURE_FOCUS_SETTLE_DELAY: Duration = Duration::from_millis(150);
const CAPTURE_TIMEOUT_TOAST: &str = "Захват комбинации отменён по таймауту.";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SettingsWindowMode {
    #[default]
    Embedded,
    Standalone,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct CaptureDialogState {
    candidate: Option<LayoutSwitchCombo>,
    error: Option<String>,
}

impl CaptureDialogState {
    fn clear(&mut self) {
        *self = Self::default();
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SelectedTextHotkeyDialogState {
    candidate: Option<SelectedTextHotkey>,
    error: Option<String>,
    shift: bool,
    ctrl: bool,
    alt: bool,
}

impl SelectedTextHotkeyDialogState {
    fn clear(&mut self) {
        *self = Self::default();
    }

    fn set_modifier(&mut self, modifier: SelectedTextHotkeyModifier, pressed: bool) {
        match modifier {
            SelectedTextHotkeyModifier::Shift => self.shift = pressed,
            SelectedTextHotkeyModifier::Ctrl => self.ctrl = pressed,
            SelectedTextHotkeyModifier::Alt => self.alt = pressed,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectedTextHotkeyModifier {
    Shift,
    Ctrl,
    Alt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectedTextHotkeyTrigger {
    Pause,
    F12,
    ScrollLock,
}

#[derive(Clone, Default)]
pub(crate) struct SettingsWindowController {
    mode: SettingsWindowMode,
    window: Rc<RefCell<Option<Rc<SettingsWindow>>>>,
}

impl SettingsWindowController {
    pub fn embedded() -> Self {
        Self {
            mode: SettingsWindowMode::Embedded,
            window: Rc::new(RefCell::new(None)),
        }
    }

    pub fn standalone() -> Self {
        Self {
            mode: SettingsWindowMode::Standalone,
            window: Rc::new(RefCell::new(None)),
        }
    }

    pub fn present(&self, app: &adw::Application) {
        if let Some(ui) = self.window.borrow().as_ref().cloned() {
            if !ui.window.is_visible() {
                ui.reload_from_daemon();
            }
            ui.window.present();
            return;
        }

        let ui = SettingsWindow::new(app, self.mode);
        {
            let window_slot = self.window.clone();
            ui.window.connect_destroy(move |_| {
                window_slot.borrow_mut().take();
            });
        }
        self.window.borrow_mut().replace(Rc::clone(&ui));
        ui.window.present();

        let ui_clone = Rc::clone(&ui);
        glib::idle_add_local_once(move || {
            initialize_window(ui_clone);
        });
    }
}

pub fn run_standalone() {
    let controller = SettingsWindowController::standalone();
    let app = adw::Application::builder()
        .application_id("org.oswitch.settings")
        .build();

    app.connect_activate(move |app| controller.present(app));
    app.run();
}

fn initialize_window(ui: Rc<SettingsWindow>) {
    let form = build_form_widgets(&ui.window);
    ui.install_form(form);

    let (event_tx, event_rx) = async_channel::unbounded();
    let presenter = SettingsPresenter::new(SettingsDbusClient, event_tx);
    ui.set_presenter(presenter.clone());

    let delay_handler = {
        let presenter = presenter.clone();
        let delay_spin = ui.delay_spin();
        delay_spin.connect_value_changed(move |spin| {
            presenter.update_layout_delay(spin.value_as_int() as u32);
        })
    };
    ui.set_delay_handler(delay_handler);

    let autostart_handler = {
        let presenter = presenter.clone();
        let autostart_switch = ui.autostart_switch();
        autostart_switch.connect_active_notify(move |switch| {
            presenter.set_autostart_enabled(switch.is_active());
        })
    };
    ui.set_autostart_handler(autostart_handler);

    let auto_switch_handler = {
        let presenter = presenter.clone();
        let auto_switch = ui.auto_switch_switch();
        auto_switch.connect_active_notify(move |switch| {
            presenter.update_auto_switch_enabled(switch.is_active());
        })
    };
    ui.set_auto_switch_handler(auto_switch_handler);

    let fix_two_capitals_handler = {
        let presenter = presenter.clone();
        let fix_two_capitals = ui.fix_two_capitals_switch();
        fix_two_capitals.connect_active_notify(move |switch| {
            presenter.update_fix_two_capitals(switch.is_active());
        })
    };
    ui.set_fix_two_capitals_handler(fix_two_capitals_handler);

    let fix_accidental_caps_lock_handler = {
        let presenter = presenter.clone();
        let fix_accidental_caps_lock = ui.fix_accidental_caps_lock_switch();
        fix_accidental_caps_lock.connect_active_notify(move |switch| {
            presenter.update_fix_accidental_caps_lock(switch.is_active());
        })
    };
    ui.set_fix_accidental_caps_lock_handler(fix_accidental_caps_lock_handler);

    let undo_handler = {
        let presenter = presenter.clone();
        let undo_dropdown = ui.undo_dropdown();
        undo_dropdown.connect_selected_notify(move |dropdown| {
            if let Some(key) = crate::model::UndoKey::ALL
                .get(dropdown.selected() as usize)
                .copied()
            {
                presenter.update_undo_key(key);
            }
        })
    };
    ui.set_undo_handler(undo_handler);

    {
        let ui = Rc::clone(&ui);
        ui.selected_text_hotkey_row().connect_activated(move |_| {
            ui.reset_selected_text_hotkey_dialog();
            ui.open_selected_text_hotkey_dialog();
        });
    }

    {
        let ui = Rc::clone(&ui);
        ui.selected_text_hotkey_dialog_cancel_button()
            .connect_clicked(move |_| {
                ui.reset_selected_text_hotkey_dialog();
                ui.close_selected_text_hotkey_dialog();
            });
    }

    {
        let presenter = presenter.clone();
        let ui = Rc::clone(&ui);
        ui.selected_text_hotkey_dialog_ok_button()
            .connect_clicked(move |_| {
                let candidate = ui.selected_text_hotkey_dialog_state.borrow().candidate;
                let Some(hotkey) = candidate else {
                    return;
                };

                presenter.update_selected_text_hotkey(hotkey);
                ui.reset_selected_text_hotkey_dialog();
                ui.close_selected_text_hotkey_dialog();
            });
    }

    {
        let ui = Rc::clone(&ui);
        ui.install_selected_text_hotkey_capture();
    }

    {
        let ui = Rc::clone(&ui);
        ui.selected_text_hotkey_dialog()
            .connect_close_request(move |dialog| {
                ui.reset_selected_text_hotkey_dialog();
                dialog.hide();
                glib::Propagation::Stop
            });
    }

    {
        let presenter = presenter.clone();
        let ui = Rc::clone(&ui);
        let label = ui.layout_switch_hint_label();
        label.connect_activate_link(move |_, uri| {
            if uri == "app://unlock-layout-switch" {
                presenter.unlock_layout_switch_override();
                return glib::Propagation::Stop;
            }

            glib::Propagation::Proceed
        });
    }

    {
        let presenter = presenter.clone();
        let ui = Rc::clone(&ui);
        ui.layout_switch_value_row().connect_activated(move |_| {
            if !ui.current_view_state().layout_switch.editable {
                return;
            }

            match presenter.start_layout_switch_capture() {
                Ok(()) => {
                    ui.reset_capture_dialog();
                    ui.open_layout_switch_dialog();
                    ui.arm_capture_timeout(presenter.clone());
                }
                Err(error) => ui.show_client_error(error, false),
            }
        });
    }

    {
        let presenter = presenter.clone();
        let ui = Rc::clone(&ui);
        ui.dialog_cancel_button().connect_clicked(move |_| {
            ui.cancel_capture_safely(&presenter, None);
        });
    }

    {
        let presenter = presenter.clone();
        let ui = Rc::clone(&ui);
        ui.dialog_ok_button().connect_clicked(move |_| {
            let candidate = ui.capture_dialog_state.borrow().candidate;
            let Some(combo) = candidate else {
                return;
            };

            ui.disarm_capture_timeout();
            match presenter.confirm_captured_layout_switch(combo) {
                Ok(()) => {
                    ui.reset_capture_dialog();
                    ui.close_layout_switch_dialog();
                }
                Err(error) => ui.show_client_error(error, false),
            }
        });
    }

    {
        let presenter = presenter.clone();
        let ui = Rc::clone(&ui);
        ui.layout_switch_dialog()
            .connect_close_request(move |dialog| {
                if ui.current_view_state().layout_switch.capture_active {
                    ui.disarm_capture_timeout();
                    ui.reset_capture_dialog();
                    if let Err(error) = presenter.cancel_layout_switch_capture() {
                        ui.show_client_error(error, false);
                    }
                }
                dialog.hide();
                glib::Propagation::Stop
            });
    }

    {
        let presenter = presenter.clone();
        let ui = Rc::clone(&ui);
        let window = ui.window.clone();
        window.connect_is_active_notify(move |_| {
            SettingsWindow::schedule_focus_loss_check(Rc::clone(&ui), presenter.clone());
        });
    }

    {
        let presenter = presenter.clone();
        let ui = Rc::clone(&ui);
        let dialog = ui.layout_switch_dialog();
        dialog.connect_is_active_notify(move |_| {
            SettingsWindow::schedule_focus_loss_check(Rc::clone(&ui), presenter.clone());
        });
    }

    {
        let presenter = presenter.clone();
        let ui = Rc::clone(&ui);
        let save_button = ui.save_button.clone();
        save_button.connect_clicked(move |_| {
            if let SaveRequest::Accepted(view_state) = presenter.save() {
                ui.apply_view_state(&view_state);
            }
        });
    }

    {
        let ui = Rc::clone(&ui);
        let presenter_for_events = presenter.clone();
        glib::MainContext::default().spawn_local(async move {
            while let Ok(event) = event_rx.recv().await {
                match event {
                    PresenterEvent::ViewStateChanged(view_state) => {
                        ui.apply_view_state(&view_state)
                    }
                    PresenterEvent::LoadFailed(error) => ui.show_client_error(error, true),
                    PresenterEvent::SaveFailed(error) => ui.show_client_error(error, false),
                    PresenterEvent::SaveSucceeded(result) => ui.show_toast(&result.message),
                    PresenterEvent::CaptureStateChanged(state) => {
                        ui.apply_capture_state(&presenter_for_events, state)
                    }
                    PresenterEvent::AutostartFailed(error) => ui.show_client_error(error, false),
                }
            }
        });
    }

    presenter.initialize();
}

fn build_form_widgets(parent_window: &adw::ApplicationWindow) -> FormWidgets {
    let autostart_row = adw::ActionRow::builder()
        .title("Автозапуск")
        .subtitle("Запускать daemon и tray через systemd --user")
        .build();
    let autostart_switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
    autostart_row.add_suffix(&autostart_switch);
    autostart_row.set_activatable_widget(Some(&autostart_switch));

    let auto_switch_row = adw::ActionRow::builder()
        .title("Автопереключение")
        .subtitle("Автоматически исправлять последнее слово при нажатии пробела")
        .build();
    let auto_switch_switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
    auto_switch_row.add_suffix(&auto_switch_switch);
    auto_switch_row.set_activatable_widget(Some(&auto_switch_switch));

    let delay_row = adw::ActionRow::builder()
        .title("Задержка переключения раскладки")
        .subtitle(format!(
            "Целое число от {} до {} мс",
            crate::model::LAYOUT_DELAY_MIN_MS,
            crate::model::LAYOUT_DELAY_MAX_MS
        ))
        .build();
    let delay_spin = gtk::SpinButton::with_range(
        crate::model::LAYOUT_DELAY_MIN_MS as f64,
        crate::model::LAYOUT_DELAY_MAX_MS as f64,
        1.0,
    );
    delay_spin.set_digits(0);
    delay_spin.set_numeric(true);
    delay_spin.set_valign(gtk::Align::Center);
    delay_spin.set_width_chars(5);
    delay_row.add_suffix(&delay_spin);
    delay_row.set_activatable_widget(Some(&delay_spin));

    let fix_two_capitals_row = adw::ActionRow::builder()
        .title("Исправлять две заглавные буквы в начале слова")
        .subtitle("Например: ПРивет -> Привет")
        .build();
    let fix_two_capitals_switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
    fix_two_capitals_row.add_suffix(&fix_two_capitals_switch);
    fix_two_capitals_row.set_activatable_widget(Some(&fix_two_capitals_switch));

    let fix_accidental_caps_lock_row = adw::ActionRow::builder()
        .title("Исправлять случайно нажатый Caps Lock")
        .subtitle("Например: hELLO -> Hello")
        .build();
    let fix_accidental_caps_lock_switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
    fix_accidental_caps_lock_row.add_suffix(&fix_accidental_caps_lock_switch);
    fix_accidental_caps_lock_row.set_activatable_widget(Some(&fix_accidental_caps_lock_switch));

    let undo_row = adw::ActionRow::builder()
        .title("Клавиша ручного исправления")
        .subtitle("Выбор горячей клавиши для ручной отмены переключения")
        .build();
    let undo_labels: Vec<&str> = crate::model::UndoKey::ALL
        .iter()
        .map(|key| key.as_str())
        .collect();
    let undo_dropdown = gtk::DropDown::from_strings(&undo_labels);
    undo_dropdown.set_valign(gtk::Align::Center);
    undo_row.add_suffix(&undo_dropdown);
    undo_row.set_activatable_widget(Some(&undo_dropdown));

    let selected_text_hotkey_row = adw::ActionRow::builder()
        .title("Горячая клавиша для выделенного текста")
        .subtitle("Копирует выделение, конвертирует раскладку и вставляет текст обратно")
        .build();
    let selected_text_hotkey_value_label = gtk::Label::new(Some("Shift+Pause"));
    selected_text_hotkey_value_label.set_halign(gtk::Align::End);
    selected_text_hotkey_value_label.set_valign(gtk::Align::Center);
    selected_text_hotkey_value_label.add_css_class("monospace");
    let selected_text_hotkey_value_icon = gtk::Image::from_icon_name("go-next-symbolic");
    selected_text_hotkey_row.add_suffix(&selected_text_hotkey_value_icon);
    selected_text_hotkey_row.add_suffix(&selected_text_hotkey_value_label);

    let selected_text_hotkey_dialog = adw::Window::builder()
        .title("Горячая клавиша для выделенного текста")
        .default_width(360)
        .default_height(220)
        .modal(true)
        .resizable(false)
        .build();
    selected_text_hotkey_dialog.set_transient_for(Some(parent_window));
    selected_text_hotkey_dialog.set_hide_on_close(true);

    let selected_hotkey_toolbar = adw::ToolbarView::new();
    let selected_hotkey_header = adw::HeaderBar::new();
    let selected_hotkey_title = adw::WindowTitle::new("Горячая клавиша", "Выделенный текст");
    selected_hotkey_header.set_title_widget(Some(&selected_hotkey_title));
    selected_hotkey_toolbar.add_top_bar(&selected_hotkey_header);

    let selected_hotkey_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    selected_hotkey_box.set_margin_top(18);
    selected_hotkey_box.set_margin_bottom(18);
    selected_hotkey_box.set_margin_start(18);
    selected_hotkey_box.set_margin_end(18);

    let selected_hotkey_heading = gtk::Label::new(Some("Нажмите горячую клавишу..."));
    selected_hotkey_heading.set_halign(gtk::Align::Start);
    selected_hotkey_heading.add_css_class("title-3");
    selected_hotkey_box.append(&selected_hotkey_heading);

    let selected_hotkey_hint = gtk::Label::new(Some(
        "Поддерживаются только сочетания Shift, Ctrl или Alt с Pause, F12 или ScrollLock.",
    ));
    selected_hotkey_hint.set_halign(gtk::Align::Start);
    selected_hotkey_hint.set_wrap(true);
    selected_hotkey_hint.add_css_class("dim-label");
    selected_hotkey_box.append(&selected_hotkey_hint);

    let selected_hotkey_current_title = gtk::Label::new(Some("Распознано"));
    selected_hotkey_current_title.set_halign(gtk::Align::Start);
    selected_hotkey_current_title.add_css_class("caption-heading");
    selected_hotkey_box.append(&selected_hotkey_current_title);

    let selected_text_hotkey_dialog_value_label = gtk::Label::new(Some("Пока не выбрана"));
    selected_text_hotkey_dialog_value_label.set_halign(gtk::Align::Start);
    selected_text_hotkey_dialog_value_label.add_css_class("monospace");
    selected_text_hotkey_dialog_value_label.add_css_class("title-4");
    selected_hotkey_box.append(&selected_text_hotkey_dialog_value_label);

    let selected_text_hotkey_dialog_error_label = gtk::Label::new(None);
    selected_text_hotkey_dialog_error_label.set_halign(gtk::Align::Start);
    selected_text_hotkey_dialog_error_label.set_wrap(true);
    selected_text_hotkey_dialog_error_label.add_css_class("error");
    selected_text_hotkey_dialog_error_label.hide();
    selected_hotkey_box.append(&selected_text_hotkey_dialog_error_label);

    let selected_hotkey_actions = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let selected_hotkey_cancel_button = gtk::Button::with_label("Отмена");
    let selected_hotkey_ok_button = gtk::Button::with_label("ОК");
    selected_hotkey_ok_button.add_css_class("suggested-action");
    selected_hotkey_ok_button.set_sensitive(false);
    let selected_hotkey_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    selected_hotkey_spacer.set_hexpand(true);
    selected_hotkey_actions.append(&selected_hotkey_cancel_button);
    selected_hotkey_actions.append(&selected_hotkey_spacer);
    selected_hotkey_actions.append(&selected_hotkey_ok_button);
    selected_hotkey_box.append(&selected_hotkey_actions);

    selected_hotkey_toolbar.set_content(Some(&selected_hotkey_box));
    selected_text_hotkey_dialog.set_content(Some(&selected_hotkey_toolbar));

    let layout_switch_value_row = adw::ActionRow::builder()
        .title("Комбинация переключения раскладки")
        .subtitle("Автоматически определяется демоном и используется для возврата раскладки назад")
        .build();
    let layout_switch_value_label = gtk::Label::new(Some("Ctrl+Shift"));
    layout_switch_value_label.set_halign(gtk::Align::End);
    layout_switch_value_label.set_valign(gtk::Align::Center);
    layout_switch_value_label.add_css_class("monospace");
    let layout_switch_value_icon = gtk::Image::from_icon_name("go-next-symbolic");
    layout_switch_value_row.add_suffix(&layout_switch_value_icon);
    layout_switch_value_row.add_suffix(&layout_switch_value_label);

    let layout_switch_dialog = adw::Window::builder()
        .title("Выбор комбинации раскладки")
        .default_width(380)
        .default_height(240)
        .modal(true)
        .resizable(false)
        .build();
    layout_switch_dialog.set_transient_for(Some(parent_window));
    layout_switch_dialog.set_hide_on_close(true);

    let dialog_toolbar = adw::ToolbarView::new();
    let dialog_header = adw::HeaderBar::new();
    let dialog_title = adw::WindowTitle::new("Выбор комбинации", "Переключение раскладки");
    dialog_header.set_title_widget(Some(&dialog_title));
    dialog_toolbar.add_top_bar(&dialog_header);

    let dialog_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    dialog_box.set_margin_top(18);
    dialog_box.set_margin_bottom(18);
    dialog_box.set_margin_start(18);
    dialog_box.set_margin_end(18);

    let dialog_heading = gtk::Label::new(Some("Нажмите комбинацию..."));
    dialog_heading.set_halign(gtk::Align::Start);
    dialog_heading.add_css_class("title-3");
    dialog_box.append(&dialog_heading);

    let dialog_warning_label = gtk::Label::new(Some(
        "Ввод сейчас перехватывается. Нажмите комбинацию или Esc для отмены. Если ничего не делать, захват автоматически отменится через 60 секунд.",
    ));
    dialog_warning_label.set_halign(gtk::Align::Start);
    dialog_warning_label.set_wrap(true);
    dialog_warning_label.add_css_class("caption");
    dialog_box.append(&dialog_warning_label);

    let dialog_capture_hint = gtk::Label::new(Some(
        "Поддерживаются Ctrl+Shift, Alt+Shift, Right Alt+Right Shift, CapsLock, Ctrl+Space, Super+Space, Left Ctrl+Left Shift, Right Ctrl+Right Shift и Left Alt+Left Shift.",
    ));
    dialog_capture_hint.set_halign(gtk::Align::Start);
    dialog_capture_hint.set_wrap(true);
    dialog_capture_hint.add_css_class("dim-label");
    dialog_box.append(&dialog_capture_hint);

    let dialog_current_title = gtk::Label::new(Some("Текущая комбинация"));
    dialog_current_title.set_halign(gtk::Align::Start);
    dialog_current_title.add_css_class("caption-heading");
    dialog_box.append(&dialog_current_title);

    let dialog_current_combo_label = gtk::Label::new(Some("Пока не выбрана"));
    dialog_current_combo_label.set_halign(gtk::Align::Start);
    dialog_current_combo_label.set_wrap(true);
    dialog_current_combo_label.add_css_class("monospace");
    dialog_current_combo_label.add_css_class("title-4");
    dialog_box.append(&dialog_current_combo_label);

    let dialog_error_label = gtk::Label::new(None);
    dialog_error_label.set_halign(gtk::Align::Start);
    dialog_error_label.set_wrap(true);
    dialog_error_label.add_css_class("error");
    dialog_error_label.hide();
    dialog_box.append(&dialog_error_label);

    let dialog_actions_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    dialog_actions_box.set_margin_top(6);

    let dialog_cancel_button = gtk::Button::with_label("Отмена");
    let dialog_ok_button = gtk::Button::with_label("ОК");
    dialog_ok_button.add_css_class("suggested-action");
    dialog_ok_button.set_sensitive(false);

    let dialog_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dialog_spacer.set_hexpand(true);

    dialog_actions_box.append(&dialog_cancel_button);
    dialog_actions_box.append(&dialog_spacer);
    dialog_actions_box.append(&dialog_ok_button);
    dialog_box.append(&dialog_actions_box);

    dialog_toolbar.set_content(Some(&dialog_box));
    layout_switch_dialog.set_content(Some(&dialog_toolbar));

    let layout_switch_hint_label = gtk::Label::new(None);
    layout_switch_hint_label.set_halign(gtk::Align::Start);
    layout_switch_hint_label.set_wrap(true);
    layout_switch_hint_label.set_use_markup(true);
    layout_switch_hint_label.set_use_underline(false);
    layout_switch_hint_label.set_selectable(false);
    layout_switch_hint_label.add_css_class("dim-label");
    layout_switch_hint_label.set_margin_top(6);
    layout_switch_hint_label.set_margin_start(12);
    layout_switch_hint_label.set_margin_end(12);
    layout_switch_hint_label.hide();
    let general_page = build_general_page(
        &autostart_row,
        &auto_switch_row,
        &delay_row,
        &fix_two_capitals_row,
        &fix_accidental_caps_lock_row,
    );
    let hotkeys_page = build_hotkeys_page(
        &undo_row,
        &selected_text_hotkey_row,
        &layout_switch_value_row,
        &layout_switch_hint_label,
    );

    let stack = gtk::Stack::builder()
        .hexpand(true)
        .vexpand(true)
        .transition_type(gtk::StackTransitionType::SlideLeftRight)
        .build();
    stack.add_titled(&general_page, Some("general"), "Общие");
    stack.add_titled(&hotkeys_page, Some("hotkeys"), "Горячие клавиши");
    stack.set_visible_child_name("general");

    let sidebar = gtk::StackSidebar::new();
    sidebar.set_stack(&stack);
    sidebar.set_vexpand(true);
    sidebar.set_width_request(180);

    let container = gtk::Box::new(gtk::Orientation::Horizontal, 18);
    container.set_margin_top(8);
    container.set_margin_bottom(12);
    container.set_margin_start(12);
    container.set_margin_end(12);
    container.append(&sidebar);
    container.append(&stack);

    FormWidgets {
        container,
        autostart_switch,
        auto_switch_switch,
        delay_spin,
        fix_two_capitals_switch,
        fix_accidental_caps_lock_switch,
        undo_dropdown,
        selected_text_hotkey_row,
        selected_text_hotkey_value_label,
        selected_text_hotkey_value_icon,
        selected_text_hotkey_dialog,
        selected_text_hotkey_dialog_value_label,
        selected_text_hotkey_dialog_error_label,
        selected_text_hotkey_dialog_ok_button: selected_hotkey_ok_button,
        selected_text_hotkey_dialog_cancel_button: selected_hotkey_cancel_button,
        layout_switch_value_row,
        layout_switch_value_label,
        layout_switch_value_icon,
        layout_switch_dialog,
        dialog_capture_hint,
        dialog_current_combo_label,
        dialog_error_label,
        dialog_ok_button,
        dialog_cancel_button,
        layout_switch_hint_label,
        autostart_handler: None,
        auto_switch_handler: None,
        delay_handler: None,
        fix_two_capitals_handler: None,
        fix_accidental_caps_lock_handler: None,
        undo_handler: None,
    }
}

fn build_general_page(
    autostart_row: &adw::ActionRow,
    auto_switch_row: &adw::ActionRow,
    delay_row: &adw::ActionRow,
    fix_two_capitals_row: &adw::ActionRow,
    fix_accidental_caps_lock_row: &adw::ActionRow,
) -> adw::Clamp {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let system_group = adw::PreferencesGroup::builder()
        .title("Система")
        .description("Параметры запуска и поведения автопереключения.")
        .build();
    system_group.add(autostart_row);
    system_group.add(auto_switch_row);
    system_group.add(delay_row);

    let corrections_group = adw::PreferencesGroup::builder()
        .title("Исправления")
        .description("Параметры коррекции регистра для уже исправленного слова.")
        .build();
    corrections_group.add(fix_two_capitals_row);
    corrections_group.add(fix_accidental_caps_lock_row);

    content.append(&system_group);
    content.append(&corrections_group);

    let clamp = adw::Clamp::new();
    clamp.set_maximum_size(PAGE_MAX_WIDTH);
    clamp.set_child(Some(&content));
    clamp
}

fn build_hotkeys_page(
    undo_row: &adw::ActionRow,
    selected_text_hotkey_row: &adw::ActionRow,
    layout_switch_value_row: &adw::ActionRow,
    layout_switch_hint_label: &gtk::Label,
) -> adw::Clamp {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let group = adw::PreferencesGroup::builder()
        .title("Горячие клавиши")
        .description("Настройка существующих горячих клавиш OpenSwitcher.")
        .build();
    group.add(undo_row);
    group.add(selected_text_hotkey_row);
    group.add(layout_switch_value_row);
    group.add(layout_switch_hint_label);

    content.append(&group);

    let clamp = adw::Clamp::new();
    clamp.set_maximum_size(PAGE_MAX_WIDTH);
    clamp.set_child(Some(&content));
    clamp
}

struct FormWidgets {
    container: gtk::Box,
    autostart_switch: gtk::Switch,
    auto_switch_switch: gtk::Switch,
    delay_spin: gtk::SpinButton,
    fix_two_capitals_switch: gtk::Switch,
    fix_accidental_caps_lock_switch: gtk::Switch,
    undo_dropdown: gtk::DropDown,
    selected_text_hotkey_row: adw::ActionRow,
    selected_text_hotkey_value_label: gtk::Label,
    selected_text_hotkey_value_icon: gtk::Image,
    selected_text_hotkey_dialog: adw::Window,
    selected_text_hotkey_dialog_value_label: gtk::Label,
    selected_text_hotkey_dialog_error_label: gtk::Label,
    selected_text_hotkey_dialog_ok_button: gtk::Button,
    selected_text_hotkey_dialog_cancel_button: gtk::Button,
    layout_switch_value_row: adw::ActionRow,
    layout_switch_value_label: gtk::Label,
    layout_switch_value_icon: gtk::Image,
    layout_switch_dialog: adw::Window,
    dialog_capture_hint: gtk::Label,
    dialog_current_combo_label: gtk::Label,
    dialog_error_label: gtk::Label,
    dialog_ok_button: gtk::Button,
    dialog_cancel_button: gtk::Button,
    layout_switch_hint_label: gtk::Label,
    autostart_handler: Option<SignalHandlerId>,
    auto_switch_handler: Option<SignalHandlerId>,
    delay_handler: Option<SignalHandlerId>,
    fix_two_capitals_handler: Option<SignalHandlerId>,
    fix_accidental_caps_lock_handler: Option<SignalHandlerId>,
    undo_handler: Option<SignalHandlerId>,
}

struct SettingsWindow {
    window: adw::ApplicationWindow,
    toast_overlay: adw::ToastOverlay,
    status_label: gtk::Label,
    form_container: gtk::Box,
    save_button: gtk::Button,
    cancel_button: gtk::Button,
    form: RefCell<Option<FormWidgets>>,
    presenter: RefCell<Option<SettingsPresenter>>,
    current_view_state: RefCell<ViewState>,
    capture_dialog_state: RefCell<CaptureDialogState>,
    selected_text_hotkey_dialog_state: RefCell<SelectedTextHotkeyDialogState>,
    capture_timeout: RefCell<Option<glib::SourceId>>,
    capture_timeout_generation: Cell<u64>,
}

impl SettingsWindow {
    fn new(app: &adw::Application, mode: SettingsWindowMode) -> Rc<Self> {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Настройки OpenSwitcher")
            .default_width(WINDOW_WIDTH)
            .default_height(WINDOW_HEIGHT)
            .resizable(false)
            .build();
        window.set_size_request(WINDOW_WIDTH, WINDOW_HEIGHT);

        let toast_overlay = adw::ToastOverlay::new();
        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let title = adw::WindowTitle::new("Настройки OpenSwitcher", "");
        header.set_title_widget(Some(&title));
        toolbar.add_top_bar(&header);

        let content_box = gtk::Box::new(gtk::Orientation::Vertical, 0);

        let status_label = gtk::Label::new(Some("Загрузка настроек из демона OpenSwitcher..."));
        status_label.set_halign(gtk::Align::Start);
        status_label.set_wrap(true);
        status_label.set_margin_top(12);
        status_label.set_margin_bottom(6);
        status_label.set_margin_start(18);
        status_label.set_margin_end(18);
        status_label.add_css_class("dim-label");
        content_box.append(&status_label);

        let form_container = gtk::Box::new(gtk::Orientation::Vertical, 0);
        form_container.set_vexpand(true);

        let form_scroller = gtk::ScrolledWindow::new();
        form_scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
        form_scroller.set_vscrollbar_policy(gtk::PolicyType::Automatic);
        form_scroller.set_hexpand(true);
        form_scroller.set_vexpand(true);
        form_scroller.set_child(Some(&form_container));
        content_box.append(&form_scroller);

        let actions_separator = gtk::Separator::new(gtk::Orientation::Horizontal);
        content_box.append(&actions_separator);

        let actions_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        actions_box.set_margin_top(12);
        actions_box.set_margin_bottom(12);
        actions_box.set_margin_start(18);
        actions_box.set_margin_end(18);

        let cancel_button = gtk::Button::with_label("Отмена");
        let save_button = gtk::Button::with_label("Сохранить");
        save_button.add_css_class("suggested-action");
        save_button.set_sensitive(false);

        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);

        actions_box.append(&cancel_button);
        actions_box.append(&spacer);
        actions_box.append(&save_button);
        content_box.append(&actions_box);

        toast_overlay.set_child(Some(&content_box));
        toolbar.set_content(Some(&toast_overlay));
        window.set_content(Some(&toolbar));

        let ui = Rc::new(Self {
            window,
            toast_overlay,
            status_label,
            form_container,
            save_button,
            cancel_button,
            form: RefCell::new(None),
            presenter: RefCell::new(None),
            current_view_state: RefCell::new(initial_view_state()),
            capture_dialog_state: RefCell::new(CaptureDialogState::default()),
            selected_text_hotkey_dialog_state: RefCell::new(
                SelectedTextHotkeyDialogState::default(),
            ),
            capture_timeout: RefCell::new(None),
            capture_timeout_generation: Cell::new(0),
        });

        if mode == SettingsWindowMode::Embedded {
            let ui_weak = Rc::downgrade(&ui);
            ui.window.connect_close_request(move |window| {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.discard_pending_changes();
                }
                window.hide();
                glib::Propagation::Stop
            });
        }

        {
            let window = ui.window.clone();
            ui.cancel_button.connect_clicked(move |_| {
                window.close();
            });
        }

        ui.apply_view_state(&initial_view_state());
        ui
    }

    fn install_form(&self, form: FormWidgets) {
        self.form_container.append(&form.container);
        self.form.replace(Some(form));
        self.apply_view_state(&initial_view_state());
        self.update_capture_dialog_widgets();
        self.update_selected_text_hotkey_dialog_widgets();
    }

    fn set_presenter(&self, presenter: SettingsPresenter) {
        self.presenter.replace(Some(presenter));
    }

    fn apply_capture_state(&self, presenter: &SettingsPresenter, state: LayoutSwitchCaptureState) {
        match state.phase {
            LayoutSwitchCapturePhase::Idle => {
                self.disarm_capture_timeout();
                presenter.sync_layout_switch_capture_active(false);
                self.reset_capture_dialog();
            }
            LayoutSwitchCapturePhase::Waiting => {
                self.capture_dialog_state.borrow_mut().clear();
                self.update_capture_dialog_widgets();
            }
            LayoutSwitchCapturePhase::Candidate => {
                if state.has_candidate {
                    let combo = state.candidate;
                    self.set_capture_candidate(combo);
                }
            }
            LayoutSwitchCapturePhase::Unsupported => {
                let message = if state.message.is_empty() {
                    "Эта комбинация сейчас не поддерживается OpenSwitcher.".to_string()
                } else {
                    state.message
                };
                self.set_capture_error(message);
            }
            LayoutSwitchCapturePhase::Cancelled => {
                self.disarm_capture_timeout();
                presenter.sync_layout_switch_capture_active(false);
                self.reset_capture_dialog();
                self.close_layout_switch_dialog();
            }
            LayoutSwitchCapturePhase::Finished => {
                self.disarm_capture_timeout();
                presenter.sync_layout_switch_capture_active(false);
                self.reset_capture_dialog();
            }
        }
    }

    fn reload_from_daemon(&self) {
        self.reset_capture_dialog();
        if let Some(presenter) = self.presenter.borrow().as_ref().cloned() {
            presenter.reload();
        }
    }

    fn discard_pending_changes(&self) {
        self.disarm_capture_timeout();
        self.reset_capture_dialog();
        if let Some(presenter) = self.presenter.borrow().as_ref().cloned() {
            if self.current_view_state().layout_switch.capture_active {
                let _ = presenter.cancel_layout_switch_capture();
            }
            presenter.discard_changes();
        }
    }

    fn set_auto_switch_handler(&self, handler: SignalHandlerId) {
        if let Some(form) = self.form.borrow_mut().as_mut() {
            form.auto_switch_handler = Some(handler);
        }
    }

    fn set_delay_handler(&self, handler: SignalHandlerId) {
        if let Some(form) = self.form.borrow_mut().as_mut() {
            form.delay_handler = Some(handler);
        }
    }

    fn set_fix_two_capitals_handler(&self, handler: SignalHandlerId) {
        if let Some(form) = self.form.borrow_mut().as_mut() {
            form.fix_two_capitals_handler = Some(handler);
        }
    }

    fn set_fix_accidental_caps_lock_handler(&self, handler: SignalHandlerId) {
        if let Some(form) = self.form.borrow_mut().as_mut() {
            form.fix_accidental_caps_lock_handler = Some(handler);
        }
    }

    fn set_autostart_handler(&self, handler: SignalHandlerId) {
        if let Some(form) = self.form.borrow_mut().as_mut() {
            form.autostart_handler = Some(handler);
        }
    }

    fn set_undo_handler(&self, handler: SignalHandlerId) {
        if let Some(form) = self.form.borrow_mut().as_mut() {
            form.undo_handler = Some(handler);
        }
    }

    fn current_view_state(&self) -> ViewState {
        self.current_view_state.borrow().clone()
    }

    fn reset_capture_dialog(&self) {
        self.capture_dialog_state.borrow_mut().clear();
        self.update_capture_dialog_widgets();
    }

    fn reset_selected_text_hotkey_dialog(&self) {
        self.selected_text_hotkey_dialog_state.borrow_mut().clear();
        self.update_selected_text_hotkey_dialog_widgets();
    }

    fn disarm_capture_timeout(&self) {
        self.capture_timeout_generation
            .set(self.capture_timeout_generation.get().wrapping_add(1));
        if let Some(source_id) = self.capture_timeout.borrow_mut().take() {
            source_id.remove();
        }
    }

    fn arm_capture_timeout(self: &Rc<Self>, presenter: SettingsPresenter) {
        if self.capture_timeout.borrow().is_some() {
            return;
        }

        let generation = self.capture_timeout_generation.get().wrapping_add(1);
        self.capture_timeout_generation.set(generation);

        let ui = Rc::clone(self);
        let source_id = glib::timeout_add_local_once(CAPTURE_TIMEOUT, move || {
            ui.capture_timeout.borrow_mut().take();

            if ui.capture_timeout_generation.get() != generation
                || !ui.current_view_state().layout_switch.capture_active
            {
                return;
            }

            ui.cancel_capture_safely(&presenter, Some(CAPTURE_TIMEOUT_TOAST));
        });

        self.capture_timeout.borrow_mut().replace(source_id);
    }

    fn schedule_focus_loss_check(self: Rc<Self>, presenter: SettingsPresenter) {
        glib::timeout_add_local_once(CAPTURE_FOCUS_SETTLE_DELAY, move || {
            if !self.current_view_state().layout_switch.capture_active {
                return;
            }

            let dialog = self.layout_switch_dialog();
            if !self.window.is_active() && !dialog.is_active() {
                self.cancel_capture_safely(&presenter, None);
            }
        });
    }

    fn cancel_capture_safely(&self, presenter: &SettingsPresenter, toast: Option<&str>) {
        self.disarm_capture_timeout();
        self.reset_capture_dialog();

        if self.current_view_state().layout_switch.capture_active {
            if let Err(error) = presenter.cancel_layout_switch_capture() {
                self.show_client_error(error, false);
            }
        }

        self.close_layout_switch_dialog();

        if let Some(message) = toast {
            self.show_toast(message);
        }
    }

    fn set_capture_candidate(&self, combo: LayoutSwitchCombo) {
        let mut state = self.capture_dialog_state.borrow_mut();
        state.candidate = Some(combo);
        state.error = None;
        drop(state);
        self.update_capture_dialog_widgets();
    }

    fn set_capture_error(&self, message: impl Into<String>) {
        let mut state = self.capture_dialog_state.borrow_mut();
        state.candidate = None;
        state.error = Some(message.into());
        drop(state);
        self.update_capture_dialog_widgets();
    }

    fn set_selected_text_hotkey_candidate(&self, hotkey: SelectedTextHotkey) {
        let mut state = self.selected_text_hotkey_dialog_state.borrow_mut();
        state.candidate = Some(hotkey);
        state.error = None;
        drop(state);
        self.update_selected_text_hotkey_dialog_widgets();
    }

    fn set_selected_text_hotkey_error(&self, message: impl Into<String>) {
        let mut state = self.selected_text_hotkey_dialog_state.borrow_mut();
        state.candidate = None;
        state.error = Some(message.into());
        drop(state);
        self.update_selected_text_hotkey_dialog_widgets();
    }

    fn update_capture_dialog_widgets(&self) {
        let state = self.capture_dialog_state.borrow().clone();
        if let Some(form) = self.form.borrow().as_ref() {
            match state.candidate {
                Some(combo) => form
                    .dialog_current_combo_label
                    .set_text(combo.short_label()),
                None => form.dialog_current_combo_label.set_text("Пока не выбрана"),
            }

            if let Some(error) = state.error.as_deref() {
                form.dialog_error_label.set_text(error);
                form.dialog_error_label.show();
            } else {
                form.dialog_error_label.hide();
            }

            form.dialog_ok_button
                .set_sensitive(state.candidate.is_some());
        }
    }

    fn update_selected_text_hotkey_dialog_widgets(&self) {
        let state = self.selected_text_hotkey_dialog_state.borrow().clone();
        if let Some(form) = self.form.borrow().as_ref() {
            match state.candidate {
                Some(hotkey) => form
                    .selected_text_hotkey_dialog_value_label
                    .set_text(hotkey.short_label()),
                None => form
                    .selected_text_hotkey_dialog_value_label
                    .set_text("Пока не выбрана"),
            }

            if let Some(error) = state.error.as_deref() {
                form.selected_text_hotkey_dialog_error_label.set_text(error);
                form.selected_text_hotkey_dialog_error_label.show();
            } else {
                form.selected_text_hotkey_dialog_error_label.hide();
            }

            form.selected_text_hotkey_dialog_ok_button
                .set_sensitive(state.candidate.is_some());
        }
    }

    fn auto_switch_switch(&self) -> gtk::Switch {
        self.form
            .borrow()
            .as_ref()
            .expect("form widgets must be installed before access")
            .auto_switch_switch
            .clone()
    }

    fn delay_spin(&self) -> gtk::SpinButton {
        self.form
            .borrow()
            .as_ref()
            .expect("form widgets must be installed before access")
            .delay_spin
            .clone()
    }

    fn fix_two_capitals_switch(&self) -> gtk::Switch {
        self.form
            .borrow()
            .as_ref()
            .expect("form widgets must be installed before access")
            .fix_two_capitals_switch
            .clone()
    }

    fn fix_accidental_caps_lock_switch(&self) -> gtk::Switch {
        self.form
            .borrow()
            .as_ref()
            .expect("form widgets must be installed before access")
            .fix_accidental_caps_lock_switch
            .clone()
    }

    fn autostart_switch(&self) -> gtk::Switch {
        self.form
            .borrow()
            .as_ref()
            .expect("form widgets must be installed before access")
            .autostart_switch
            .clone()
    }

    fn undo_dropdown(&self) -> gtk::DropDown {
        self.form
            .borrow()
            .as_ref()
            .expect("form widgets must be installed before access")
            .undo_dropdown
            .clone()
    }

    fn selected_text_hotkey_row(&self) -> adw::ActionRow {
        self.form
            .borrow()
            .as_ref()
            .expect("form widgets must be installed before access")
            .selected_text_hotkey_row
            .clone()
    }

    fn selected_text_hotkey_dialog(&self) -> adw::Window {
        self.form
            .borrow()
            .as_ref()
            .expect("form widgets must be installed before access")
            .selected_text_hotkey_dialog
            .clone()
    }

    fn selected_text_hotkey_dialog_ok_button(&self) -> gtk::Button {
        self.form
            .borrow()
            .as_ref()
            .expect("form widgets must be installed before access")
            .selected_text_hotkey_dialog_ok_button
            .clone()
    }

    fn selected_text_hotkey_dialog_cancel_button(&self) -> gtk::Button {
        self.form
            .borrow()
            .as_ref()
            .expect("form widgets must be installed before access")
            .selected_text_hotkey_dialog_cancel_button
            .clone()
    }

    fn layout_switch_value_row(&self) -> adw::ActionRow {
        self.form
            .borrow()
            .as_ref()
            .expect("form widgets must be installed before access")
            .layout_switch_value_row
            .clone()
    }

    fn layout_switch_dialog(&self) -> adw::Window {
        self.form
            .borrow()
            .as_ref()
            .expect("form widgets must be installed before access")
            .layout_switch_dialog
            .clone()
    }

    fn dialog_ok_button(&self) -> gtk::Button {
        self.form
            .borrow()
            .as_ref()
            .expect("form widgets must be installed before access")
            .dialog_ok_button
            .clone()
    }

    fn dialog_cancel_button(&self) -> gtk::Button {
        self.form
            .borrow()
            .as_ref()
            .expect("form widgets must be installed before access")
            .dialog_cancel_button
            .clone()
    }

    fn layout_switch_hint_label(&self) -> gtk::Label {
        self.form
            .borrow()
            .as_ref()
            .expect("form widgets must be installed before access")
            .layout_switch_hint_label
            .clone()
    }

    fn open_layout_switch_dialog(&self) {
        if let Some(form) = self.form.borrow().as_ref() {
            self.reset_capture_dialog();
            form.layout_switch_dialog.present();
        }
    }

    fn open_selected_text_hotkey_dialog(&self) {
        if let Some(form) = self.form.borrow().as_ref() {
            self.reset_selected_text_hotkey_dialog();
            form.selected_text_hotkey_dialog.present();
        }
    }

    fn close_layout_switch_dialog(&self) {
        if let Some(form) = self.form.borrow().as_ref() {
            form.layout_switch_dialog.hide();
        }
    }

    fn close_selected_text_hotkey_dialog(&self) {
        if let Some(form) = self.form.borrow().as_ref() {
            form.selected_text_hotkey_dialog.hide();
        }
    }

    fn install_selected_text_hotkey_capture(self: &Rc<Self>) {
        let controller = gtk::EventControllerKey::new();

        {
            let ui = Rc::clone(self);
            controller.connect_key_pressed(move |_, key, _, _| {
                if key == gdk::Key::Escape {
                    ui.reset_selected_text_hotkey_dialog();
                    ui.close_selected_text_hotkey_dialog();
                    return glib::Propagation::Stop;
                }

                if let Some(modifier) = selected_text_hotkey_modifier_from_key(key) {
                    ui.selected_text_hotkey_dialog_state
                        .borrow_mut()
                        .set_modifier(modifier, true);
                    return glib::Propagation::Stop;
                }

                if let Some(trigger) = selected_text_hotkey_trigger_from_key(key) {
                    let state = ui.selected_text_hotkey_dialog_state.borrow().clone();
                    match selected_text_hotkey_from_capture_state(&state, trigger) {
                        Ok(hotkey) => ui.set_selected_text_hotkey_candidate(hotkey),
                        Err(message) => ui.set_selected_text_hotkey_error(message),
                    }
                    return glib::Propagation::Stop;
                }

                ui.set_selected_text_hotkey_error(
                    "Поддерживаются только сочетания Shift, Ctrl или Alt с Pause, F12 или ScrollLock.",
                );
                glib::Propagation::Stop
            });
        }

        {
            let ui = Rc::clone(self);
            controller.connect_key_released(move |_, key, _, _| {
                let Some(modifier) = selected_text_hotkey_modifier_from_key(key) else {
                    return;
                };

                ui.selected_text_hotkey_dialog_state
                    .borrow_mut()
                    .set_modifier(modifier, false);
            });
        }

        self.selected_text_hotkey_dialog()
            .add_controller(controller);
    }

    fn apply_view_state(&self, state: &ViewState) {
        *self.current_view_state.borrow_mut() = state.clone();

        if let Some(form) = self.form.borrow().as_ref() {
            if let Some(autostart_handler) = &form.autostart_handler {
                form.autostart_switch.block_signal(autostart_handler);
            }
            form.autostart_switch.set_active(state.autostart_enabled);
            if let Some(autostart_handler) = &form.autostart_handler {
                form.autostart_switch.unblock_signal(autostart_handler);
            }

            if let Some(auto_switch_handler) = &form.auto_switch_handler {
                form.auto_switch_switch.block_signal(auto_switch_handler);
            }
            form.auto_switch_switch
                .set_active(state.auto_switch_enabled);
            if let Some(auto_switch_handler) = &form.auto_switch_handler {
                form.auto_switch_switch.unblock_signal(auto_switch_handler);
            }

            if let Some(delay_handler) = &form.delay_handler {
                form.delay_spin.block_signal(delay_handler);
            }
            form.delay_spin.set_value(state.layout_delay_ms as f64);
            if let Some(delay_handler) = &form.delay_handler {
                form.delay_spin.unblock_signal(delay_handler);
            }

            if let Some(fix_two_capitals_handler) = &form.fix_two_capitals_handler {
                form.fix_two_capitals_switch
                    .block_signal(fix_two_capitals_handler);
            }
            form.fix_two_capitals_switch
                .set_active(state.fix_two_capitals);
            if let Some(fix_two_capitals_handler) = &form.fix_two_capitals_handler {
                form.fix_two_capitals_switch
                    .unblock_signal(fix_two_capitals_handler);
            }

            if let Some(fix_accidental_caps_lock_handler) = &form.fix_accidental_caps_lock_handler {
                form.fix_accidental_caps_lock_switch
                    .block_signal(fix_accidental_caps_lock_handler);
            }
            form.fix_accidental_caps_lock_switch
                .set_active(state.fix_accidental_caps_lock);
            if let Some(fix_accidental_caps_lock_handler) = &form.fix_accidental_caps_lock_handler {
                form.fix_accidental_caps_lock_switch
                    .unblock_signal(fix_accidental_caps_lock_handler);
            }

            if let Some(undo_handler) = &form.undo_handler {
                form.undo_dropdown.block_signal(undo_handler);
            }
            let undo_index = crate::model::UndoKey::ALL
                .iter()
                .position(|key| *key == state.undo_key)
                .unwrap_or(0);
            form.undo_dropdown.set_selected(undo_index as u32);
            if let Some(undo_handler) = &form.undo_handler {
                form.undo_dropdown.unblock_signal(undo_handler);
            }

            form.autostart_switch.set_sensitive(state.form_enabled);
            form.auto_switch_switch.set_sensitive(state.form_enabled);
            form.delay_spin.set_sensitive(state.form_enabled);
            form.fix_two_capitals_switch
                .set_sensitive(state.form_enabled);
            form.fix_accidental_caps_lock_switch
                .set_sensitive(state.form_enabled);
            form.undo_dropdown.set_sensitive(state.form_enabled);
            form.selected_text_hotkey_row
                .set_sensitive(state.form_enabled);
            form.selected_text_hotkey_row
                .set_activatable(state.form_enabled);
            form.selected_text_hotkey_value_icon
                .set_visible(state.form_enabled);
            form.selected_text_hotkey_value_label
                .set_text(state.selected_text_hotkey.short_label());

            form.layout_switch_value_label
                .set_text(&state.layout_switch.combo_label);

            let manual_actions_enabled = state.layout_switch.editable;
            let row_is_actionable =
                state.layout_switch.actions.can_capture && manual_actions_enabled;
            form.layout_switch_value_row
                .set_activatable(row_is_actionable);
            form.layout_switch_value_row
                .set_sensitive(state.form_enabled);
            form.layout_switch_value_icon.set_visible(row_is_actionable);

            form.dialog_capture_hint
                .set_text(if state.layout_switch.capture_active {
                    "Поддерживаемые варианты: Ctrl+Shift, Alt+Shift, Right Alt+Right Shift, CapsLock, Ctrl+Space, Super+Space, Left Ctrl+Left Shift, Right Ctrl+Right Shift и Left Alt+Left Shift."
                } else {
                    "Поддерживаемые варианты: Ctrl+Shift, Alt+Shift, Right Alt+Right Shift, CapsLock, Ctrl+Space, Super+Space, Left Ctrl+Left Shift, Right Ctrl+Right Shift и Left Alt+Left Shift."
                });

            if state.layout_switch.show_unlock_hint {
                form.layout_switch_hint_label.set_markup(
                    "Раскладка определена автоматически. Если мы определили неправильно, нажмите <a href=\"app://unlock-layout-switch\">сюда</a>.",
                );
                form.layout_switch_hint_label.show();
            } else if state.layout_switch.show_fallback_hint {
                form.layout_switch_hint_label
                    .set_text(&state.layout_switch.fallback_hint_text);
                form.layout_switch_hint_label.show();
            } else {
                form.layout_switch_hint_label.hide();
            }
        }

        self.save_button.set_sensitive(state.save_enabled);
        self.cancel_button.set_sensitive(state.cancel_enabled);
        self.status_label.set_text(state.status_text);
    }

    fn show_toast(&self, message: &str) {
        self.toast_overlay.add_toast(adw::Toast::new(message));
    }

    fn show_error(&self, heading: &str, body: &str) {
        let dialog = adw::MessageDialog::builder()
            .transient_for(&self.window)
            .heading(heading)
            .body(body)
            .build();
        dialog.add_response("ok", "ОК");
        dialog.set_default_response(Some("ok"));
        dialog.set_close_response("ok");
        dialog.present();
    }

    fn show_client_error(&self, error: SettingsClientError, loading: bool) {
        let (heading, body) = describe_client_error(&error, loading);
        self.show_error(heading, &body);
    }
}

fn initial_view_state() -> ViewState {
    ViewState {
        autostart_enabled: false,
        auto_switch_enabled: crate::model::Settings::default().auto_switch_enabled,
        fix_two_capitals: crate::model::Settings::default().fix_two_capitals,
        fix_accidental_caps_lock: crate::model::Settings::default().fix_accidental_caps_lock,
        layout_delay_ms: crate::model::Settings::default().layout_delay_ms,
        undo_key: crate::model::Settings::default().undo_key,
        selected_text_hotkey: crate::model::Settings::default().selected_text_hotkey,
        layout_switch: LayoutSwitchViewState {
            combo: crate::model::Settings::default().layout_switch.combo,
            combo_label: crate::model::Settings::default()
                .layout_switch
                .combo
                .short_label()
                .to_string(),
            source: crate::model::Settings::default().layout_switch.source,
            editable: false,
            manual_override_active: false,
            show_unlock_hint: false,
            show_fallback_hint: false,
            fallback_hint_text: String::new(),
            capture_active: false,
            capture_hint: "",
            actions: LayoutSwitchActionsState { can_capture: false },
        },
        loading: true,
        saving: false,
        loaded: false,
        dirty: false,
        form_enabled: false,
        save_enabled: false,
        cancel_enabled: true,
        status_text: "Загрузка настроек из демона OpenSwitcher...",
    }
}

fn selected_text_hotkey_modifier_from_key(key: gdk::Key) -> Option<SelectedTextHotkeyModifier> {
    match key {
        gdk::Key::Shift_L | gdk::Key::Shift_R => Some(SelectedTextHotkeyModifier::Shift),
        gdk::Key::Control_L | gdk::Key::Control_R => Some(SelectedTextHotkeyModifier::Ctrl),
        gdk::Key::Alt_L | gdk::Key::Alt_R => Some(SelectedTextHotkeyModifier::Alt),
        _ => None,
    }
}

fn selected_text_hotkey_trigger_from_key(key: gdk::Key) -> Option<SelectedTextHotkeyTrigger> {
    match key {
        gdk::Key::Pause => Some(SelectedTextHotkeyTrigger::Pause),
        gdk::Key::F12 => Some(SelectedTextHotkeyTrigger::F12),
        gdk::Key::Scroll_Lock => Some(SelectedTextHotkeyTrigger::ScrollLock),
        _ => None,
    }
}

fn selected_text_hotkey_from_capture_state(
    state: &SelectedTextHotkeyDialogState,
    trigger: SelectedTextHotkeyTrigger,
) -> Result<SelectedTextHotkey, &'static str> {
    let modifier_count =
        usize::from(state.shift) + usize::from(state.ctrl) + usize::from(state.alt);
    if modifier_count != 1 {
        return Err("Нужна ровно одна клавиша-модификатор: Shift, Ctrl или Alt.");
    }

    Ok(match (state.shift, state.ctrl, state.alt, trigger) {
        (true, false, false, SelectedTextHotkeyTrigger::Pause) => SelectedTextHotkey::ShiftPause,
        (false, true, false, SelectedTextHotkeyTrigger::Pause) => SelectedTextHotkey::CtrlPause,
        (false, false, true, SelectedTextHotkeyTrigger::Pause) => SelectedTextHotkey::AltPause,
        (true, false, false, SelectedTextHotkeyTrigger::F12) => SelectedTextHotkey::ShiftF12,
        (false, true, false, SelectedTextHotkeyTrigger::F12) => SelectedTextHotkey::CtrlF12,
        (false, false, true, SelectedTextHotkeyTrigger::F12) => SelectedTextHotkey::AltF12,
        (true, false, false, SelectedTextHotkeyTrigger::ScrollLock) => {
            SelectedTextHotkey::ShiftScrollLock
        }
        (false, true, false, SelectedTextHotkeyTrigger::ScrollLock) => {
            SelectedTextHotkey::CtrlScrollLock
        }
        (false, false, true, SelectedTextHotkeyTrigger::ScrollLock) => {
            SelectedTextHotkey::AltScrollLock
        }
        _ => {
            return Err(
                "Поддерживаются только сочетания Shift, Ctrl или Alt с Pause, F12 или ScrollLock.",
            )
        }
    })
}

fn describe_client_error(error: &SettingsClientError, loading: bool) -> (&'static str, String) {
    match error {
        SettingsClientError::Connection(source) => (
            if loading {
                "Не удалось подключиться к D-Bus"
            } else {
                "Нет соединения с D-Bus"
            },
            format!("OpenSwitcher не смог открыть session D-Bus.\n\n{source}"),
        ),
        SettingsClientError::Proxy(source) => (
            "Не удалось создать D-Bus proxy",
            format!("Не удалось подготовить запрос к демону OpenSwitcher.\n\n{source}"),
        ),
        SettingsClientError::Daemon(source) => (
            if loading {
                "Демон не вернул настройки"
            } else {
                "Демон отклонил сохранение"
            },
            source.to_string(),
        ),
        SettingsClientError::ServiceManager(source) => (
            "Не удалось изменить автозапуск",
            format!("OpenSwitcher не смог выполнить команду systemd user-сервисов.\n\n{source}"),
        ),
        SettingsClientError::Validation(error) => ("Некорректные значения", error.to_string()),
    }
}
