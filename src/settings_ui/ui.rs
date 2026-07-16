use super::dbus_client::SettingsDbusClient;
use super::presenter::{PresenterEvent, SaveRequest, SettingsPresenter};
use super::state::{LayoutSwitchActionsState, LayoutSwitchViewState, ViewState};
use crate::error::SettingsClientError;
use crate::model::{
    DesktopEnvironment, HotkeyModifiers, HotkeySpec, HotkeyTrigger, LayoutSwitchCapturePhase,
    LayoutSwitchCaptureState, LayoutSwitchCombo, LayoutSwitchSource, SessionType, SystemContext,
};
use adw::prelude::*;
use gtk::gdk;
use gtk::glib::{self, SignalHandlerId};
use std::cell::{Cell, RefCell};
use std::fs;
use std::rc::Rc;
use std::time::Duration;

const WINDOW_WIDTH: i32 = 760;
const WINDOW_HEIGHT: i32 = 520;
const PAGE_MAX_WIDTH: i32 = 560;
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(60);
const CAPTURE_FOCUS_SETTLE_DELAY: Duration = Duration::from_millis(150);
const CAPTURE_TIMEOUT_TOAST: &str = "Захват комбинации отменён по таймауту.";
const GITHUB_URL: &str = "https://github.com/andreymukha/OpenSwitcher";

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
struct HotkeyDialogState {
    target: HotkeyDialogTarget,
    candidate: Option<HotkeySpec>,
    error: Option<String>,
    shift: bool,
    ctrl: bool,
    alt: bool,
}

impl HotkeyDialogState {
    fn set_target(&mut self, target: HotkeyDialogTarget) {
        *self = Self {
            target,
            ..Self::default()
        };
    }

    fn modifiers(&self) -> HotkeyModifiers {
        HotkeyModifiers::new(self.shift, self.ctrl, self.alt)
    }

    fn set_modifier(&mut self, modifier: HotkeyModifier, pressed: bool) {
        match modifier {
            HotkeyModifier::Shift => self.shift = pressed,
            HotkeyModifier::Ctrl => self.ctrl = pressed,
            HotkeyModifier::Alt => self.alt = pressed,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HotkeyDialogTarget {
    ManualCorrection,
    SelectedText,
}

impl Default for HotkeyDialogTarget {
    fn default() -> Self {
        Self::SelectedText
    }
}

impl HotkeyDialogTarget {
    fn dialog_subtitle(self) -> &'static str {
        match self {
            Self::ManualCorrection => "Ручное исправление",
            Self::SelectedText => "Выделенный текст",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HotkeyModifier {
    Shift,
    Ctrl,
    Alt,
}

// Window controller
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

// Initialization and event wiring
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
    let client = match SettingsDbusClient::connect() {
        Ok(client) => client,
        Err(error) => {
            ui.show_client_error(error, true);
            return;
        }
    };
    let presenter = SettingsPresenter::new(client, event_tx);
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

    {
        let presenter = presenter.clone();
        let ui = Rc::clone(&ui);
        ui.manual_hotkey_row().connect_activated(move |_| {
            ui.open_hotkey_dialog_for(&presenter, HotkeyDialogTarget::ManualCorrection);
        });
    }

    {
        let presenter = presenter.clone();
        let ui = Rc::clone(&ui);
        ui.selected_text_hotkey_row().connect_activated(move |_| {
            ui.open_hotkey_dialog_for(&presenter, HotkeyDialogTarget::SelectedText);
        });
    }

    {
        let ui = Rc::clone(&ui);
        ui.selected_text_hotkey_dialog_cancel_button()
            .connect_clicked(move |_| {
                ui.reset_hotkey_dialog(hotkey_dialog_target(&ui.hotkey_dialog_state));
                ui.close_hotkey_dialog();
            });
    }

    {
        let presenter = presenter.clone();
        let ui = Rc::clone(&ui);
        ui.selected_text_hotkey_dialog_ok_button()
            .connect_clicked(move |_| {
                let state = ui.hotkey_dialog_state.borrow().clone();
                let candidate = state.candidate;
                let Some(hotkey) = candidate else {
                    return;
                };

                let duplicate_error = {
                    let view_state = ui.current_view_state.borrow();
                    duplicate_hotkey_dialog_error(state.target, hotkey, &view_state)
                };
                if let Some(message) = duplicate_error {
                    ui.set_hotkey_candidate_error(hotkey, message);
                    return;
                }

                match state.target {
                    HotkeyDialogTarget::ManualCorrection => {
                        presenter.update_manual_correction_hotkey(hotkey);
                    }
                    HotkeyDialogTarget::SelectedText => {
                        presenter.update_selected_text_hotkey(hotkey);
                    }
                }
                ui.reset_hotkey_dialog(state.target);
                ui.close_hotkey_dialog();
            });
    }

    {
        let ui = Rc::clone(&ui);
        ui.install_hotkey_capture();
    }

    {
        let ui = Rc::clone(&ui);
        ui.selected_text_hotkey_dialog()
            .connect_close_request(move |_| {
                ui.reset_hotkey_dialog(hotkey_dialog_target(&ui.hotkey_dialog_state));
                ui.close_hotkey_dialog();
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
                Err(error) => ui.handle_capture_failed(error),
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
                Err(error) => ui.handle_capture_failed(error),
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
        let ui = Rc::downgrade(&ui);
        glib::MainContext::default().spawn_local(async move {
            while let Ok(event) = event_rx.recv().await {
                let Some(ui) = ui.upgrade() else {
                    break;
                };
                match event {
                    PresenterEvent::ViewStateChanged(view_state) => {
                        ui.apply_view_state(&view_state)
                    }
                    PresenterEvent::LoadFailed(error) => ui.show_client_error(error, true),
                    PresenterEvent::SaveFailed(error) => ui.show_client_error(error, false),
                    PresenterEvent::SaveSucceeded(result) => ui.show_toast(&result.message),
                    PresenterEvent::CaptureStateChanged { generation, state } => {
                        let presenter = ui.presenter.borrow().as_ref().cloned();
                        if let Some(presenter) = presenter {
                            if presenter.apply_capture_state_event(generation, &state) {
                                ui.apply_capture_state(state);
                            }
                        }
                    }
                    PresenterEvent::CaptureRenewFailed { generation, error } => {
                        let presenter = ui.presenter.borrow().as_ref().cloned();
                        if let Some(presenter) = presenter {
                            if presenter.apply_capture_renew_failure(generation) {
                                ui.handle_capture_failed(error);
                            }
                        }
                    }
                    PresenterEvent::AutostartFailed(error) => ui.show_client_error(error, false),
                }
            }
        });
    }

    presenter.initialize();
}

// Form/widget construction
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
        .title("Горячая клавиша ручного исправления")
        .subtitle("Исправляет слово перед курсором или отменяет последнее переключение")
        .build();
    let manual_hotkey_value_label = gtk::Label::new(Some("Pause"));
    manual_hotkey_value_label.set_halign(gtk::Align::End);
    manual_hotkey_value_label.set_valign(gtk::Align::Center);
    manual_hotkey_value_label.add_css_class("monospace");
    let manual_hotkey_value_icon = gtk::Image::from_icon_name("go-next-symbolic");
    undo_row.add_suffix(&manual_hotkey_value_icon);
    undo_row.add_suffix(&manual_hotkey_value_label);

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
        .title("Горячая клавиша")
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

    let hotkey_dialog_content_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    hotkey_dialog_content_box.set_margin_top(18);
    hotkey_dialog_content_box.set_margin_bottom(18);
    hotkey_dialog_content_box.set_margin_start(18);
    hotkey_dialog_content_box.set_margin_end(18);
    hotkey_dialog_content_box.set_focusable(true);

    let selected_hotkey_heading = gtk::Label::new(Some("Нажмите горячую клавишу..."));
    selected_hotkey_heading.set_halign(gtk::Align::Start);
    selected_hotkey_heading.add_css_class("title-3");
    hotkey_dialog_content_box.append(&selected_hotkey_heading);

    let selected_hotkey_hint = gtk::Label::new(Some(
        "Поддерживаются F9, F10, F12, Pause, ScrollLock, Insert или Menu с 0-3 модификаторами Shift, Ctrl и Alt.",
    ));
    selected_hotkey_hint.set_halign(gtk::Align::Start);
    selected_hotkey_hint.set_wrap(true);
    selected_hotkey_hint.add_css_class("dim-label");
    hotkey_dialog_content_box.append(&selected_hotkey_hint);

    let selected_hotkey_current_title = gtk::Label::new(Some("Распознано"));
    selected_hotkey_current_title.set_halign(gtk::Align::Start);
    selected_hotkey_current_title.add_css_class("caption-heading");
    hotkey_dialog_content_box.append(&selected_hotkey_current_title);

    let selected_text_hotkey_dialog_value_label = gtk::Label::new(Some("Пока не выбрана"));
    selected_text_hotkey_dialog_value_label.set_halign(gtk::Align::Start);
    selected_text_hotkey_dialog_value_label.add_css_class("monospace");
    selected_text_hotkey_dialog_value_label.add_css_class("title-4");
    hotkey_dialog_content_box.append(&selected_text_hotkey_dialog_value_label);

    let selected_text_hotkey_dialog_error_label = gtk::Label::new(None);
    selected_text_hotkey_dialog_error_label.set_halign(gtk::Align::Start);
    selected_text_hotkey_dialog_error_label.set_wrap(true);
    selected_text_hotkey_dialog_error_label.add_css_class("error");
    selected_text_hotkey_dialog_error_label.hide();
    hotkey_dialog_content_box.append(&selected_text_hotkey_dialog_error_label);

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
    hotkey_dialog_content_box.append(&selected_hotkey_actions);

    selected_hotkey_toolbar.set_content(Some(&hotkey_dialog_content_box));
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

    let distro_pretty_name = current_distro_pretty_name();
    let (about_version_row, _about_version_value_label) =
        build_about_value_row("Версия", env!("CARGO_PKG_VERSION"));
    let (about_os_row, _about_os_value_label) =
        build_about_value_row("ОС", operating_system_label());
    let (about_distro_row, _about_distro_value_label) =
        build_about_value_row("Дистрибутив", &distro_pretty_name);
    let (about_arch_row, _about_arch_value_label) =
        build_about_value_row("Архитектура", std::env::consts::ARCH);
    let (about_session_row, about_session_value_label) = build_about_value_row("Сессия", "Unknown");
    let (about_desktop_row, about_desktop_value_label) =
        build_about_value_row("Рабочее окружение", "Unknown");
    let (about_layout_combo_row, about_layout_combo_value_label) =
        build_about_value_row("Комбинация переключения", "Unknown");
    let (about_layout_source_row, about_layout_source_value_label) =
        build_about_value_row("Источник определения", "Unknown");
    let (about_license_row, _about_license_value_label) = build_about_value_row("Лицензия", "MIT");
    let (about_github_row, _about_github_value_label) = build_about_value_row("GitHub", GITHUB_URL);

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

    let hotkey_warning_label = gtk::Label::new(None);
    hotkey_warning_label.set_halign(gtk::Align::Start);
    hotkey_warning_label.set_wrap(true);
    hotkey_warning_label.add_css_class("warning");
    hotkey_warning_label.set_margin_top(8);
    hotkey_warning_label.set_margin_bottom(2);
    hotkey_warning_label.set_margin_start(12);
    hotkey_warning_label.set_margin_end(12);
    hotkey_warning_label.hide();
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
        &hotkey_warning_label,
        &layout_switch_value_row,
        &layout_switch_hint_label,
    );
    let about_page = build_about_page(AboutPageRows {
        app: [
            &about_version_row,
            &about_os_row,
            &about_distro_row,
            &about_arch_row,
        ],
        runtime: [
            &about_session_row,
            &about_desktop_row,
            &about_layout_combo_row,
            &about_layout_source_row,
        ],
        project: [&about_license_row, &about_github_row],
    });

    let stack = gtk::Stack::builder()
        .hexpand(true)
        .vexpand(true)
        .transition_type(gtk::StackTransitionType::SlideLeftRight)
        .build();
    stack.set_hhomogeneous(false);
    stack.set_vhomogeneous(false);
    stack.add_titled(&general_page, Some("general"), "Общие");
    stack.add_titled(&hotkeys_page, Some("hotkeys"), "Горячие клавиши");
    stack.add_titled(&about_page, Some("about"), "О программе");
    stack.set_visible_child_name("general");

    let page_scroller = gtk::ScrolledWindow::new();
    page_scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
    page_scroller.set_vscrollbar_policy(gtk::PolicyType::Automatic);
    page_scroller.set_hexpand(true);
    page_scroller.set_vexpand(true);
    page_scroller.set_child(Some(&stack));

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
    container.append(&page_scroller);

    FormWidgets {
        container,
        autostart_switch,
        auto_switch_switch,
        delay_spin,
        fix_two_capitals_switch,
        fix_accidental_caps_lock_switch,
        manual_hotkey_row: undo_row,
        manual_hotkey_value_label,
        manual_hotkey_value_icon,
        selected_text_hotkey_row,
        selected_text_hotkey_value_label,
        selected_text_hotkey_value_icon,
        selected_text_hotkey_dialog,
        selected_text_hotkey_title: selected_hotkey_title,
        hotkey_dialog_content_box,
        selected_text_hotkey_dialog_value_label,
        selected_text_hotkey_dialog_error_label,
        selected_text_hotkey_dialog_ok_button: selected_hotkey_ok_button,
        selected_text_hotkey_dialog_cancel_button: selected_hotkey_cancel_button,
        layout_switch_value_row,
        layout_switch_value_label,
        layout_switch_value_icon,
        about_session_value_label,
        about_desktop_value_label,
        about_layout_combo_value_label,
        about_layout_source_value_label,
        layout_switch_dialog,
        dialog_capture_hint,
        dialog_current_combo_label,
        dialog_error_label,
        dialog_ok_button,
        dialog_cancel_button,
        hotkey_warning_label,
        layout_switch_hint_label,
        autostart_handler: None,
        auto_switch_handler: None,
        delay_handler: None,
        fix_two_capitals_handler: None,
        fix_accidental_caps_lock_handler: None,
    }
}

// Page builders
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
    corrections_group.set_margin_top(20);
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
    hotkey_warning_label: &gtk::Label,
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
    group.add(hotkey_warning_label);
    group.add(layout_switch_value_row);
    group.add(layout_switch_hint_label);

    content.append(&group);

    let clamp = adw::Clamp::new();
    clamp.set_maximum_size(PAGE_MAX_WIDTH);
    clamp.set_child(Some(&content));
    clamp
}

struct AboutPageRows<'a> {
    app: [&'a adw::ActionRow; 4],
    runtime: [&'a adw::ActionRow; 4],
    project: [&'a adw::ActionRow; 2],
}

fn build_about_page(rows: AboutPageRows<'_>) -> adw::Clamp {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let app_group = adw::PreferencesGroup::builder()
        .title("OpenSwitcher")
        .description("Сведения о приложении и текущем окружении.")
        .build();
    for row in rows.app {
        app_group.add(row);
    }

    let runtime_group = adw::PreferencesGroup::builder()
        .title("Окружение")
        .description("Данные, которые OpenSwitcher использует для работы с раскладкой.")
        .build();
    runtime_group.set_margin_top(20);
    for row in rows.runtime {
        runtime_group.add(row);
    }

    let project_group = adw::PreferencesGroup::builder().title("Проект").build();
    project_group.set_margin_top(20);
    for row in rows.project {
        project_group.add(row);
    }

    content.append(&app_group);
    content.append(&runtime_group);
    content.append(&project_group);

    let clamp = adw::Clamp::new();
    clamp.set_maximum_size(PAGE_MAX_WIDTH);
    clamp.set_child(Some(&content));
    clamp
}

fn build_about_value_row(title: &str, value: &str) -> (adw::ActionRow, gtk::Label) {
    let row = adw::ActionRow::builder().title(title).build();
    let value_label = gtk::Label::new(Some(value));
    value_label.set_halign(gtk::Align::End);
    value_label.set_valign(gtk::Align::Center);
    value_label.set_selectable(true);
    value_label.set_wrap(true);
    value_label.set_xalign(1.0);
    value_label.add_css_class("monospace");
    row.add_suffix(&value_label);
    (row, value_label)
}

// Settings window widgets
struct FormWidgets {
    container: gtk::Box,
    autostart_switch: gtk::Switch,
    auto_switch_switch: gtk::Switch,
    delay_spin: gtk::SpinButton,
    fix_two_capitals_switch: gtk::Switch,
    fix_accidental_caps_lock_switch: gtk::Switch,
    manual_hotkey_row: adw::ActionRow,
    manual_hotkey_value_label: gtk::Label,
    manual_hotkey_value_icon: gtk::Image,
    selected_text_hotkey_row: adw::ActionRow,
    selected_text_hotkey_value_label: gtk::Label,
    selected_text_hotkey_value_icon: gtk::Image,
    selected_text_hotkey_dialog: adw::Window,
    selected_text_hotkey_title: adw::WindowTitle,
    hotkey_dialog_content_box: gtk::Box,
    selected_text_hotkey_dialog_value_label: gtk::Label,
    selected_text_hotkey_dialog_error_label: gtk::Label,
    selected_text_hotkey_dialog_ok_button: gtk::Button,
    selected_text_hotkey_dialog_cancel_button: gtk::Button,
    layout_switch_value_row: adw::ActionRow,
    layout_switch_value_label: gtk::Label,
    layout_switch_value_icon: gtk::Image,
    about_session_value_label: gtk::Label,
    about_desktop_value_label: gtk::Label,
    about_layout_combo_value_label: gtk::Label,
    about_layout_source_value_label: gtk::Label,
    layout_switch_dialog: adw::Window,
    dialog_capture_hint: gtk::Label,
    dialog_current_combo_label: gtk::Label,
    dialog_error_label: gtk::Label,
    dialog_ok_button: gtk::Button,
    dialog_cancel_button: gtk::Button,
    hotkey_warning_label: gtk::Label,
    layout_switch_hint_label: gtk::Label,
    autostart_handler: Option<SignalHandlerId>,
    auto_switch_handler: Option<SignalHandlerId>,
    delay_handler: Option<SignalHandlerId>,
    fix_two_capitals_handler: Option<SignalHandlerId>,
    fix_accidental_caps_lock_handler: Option<SignalHandlerId>,
}

// Settings window
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
    hotkey_dialog_state: RefCell<HotkeyDialogState>,
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
        content_box.append(&form_container);

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
            hotkey_dialog_state: RefCell::new(HotkeyDialogState::default()),
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
        } else {
            let ui_weak = Rc::downgrade(&ui);
            ui.window.connect_close_request(move |_| {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.discard_pending_changes();
                }
                glib::Propagation::Proceed
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
        self.update_hotkey_dialog_widgets();
    }

    fn set_presenter(&self, presenter: SettingsPresenter) {
        self.presenter.replace(Some(presenter));
    }

    // Capture dialogs
    fn apply_capture_state(&self, state: LayoutSwitchCaptureState) {
        match state.phase {
            LayoutSwitchCapturePhase::Idle => {
                self.disarm_capture_timeout();
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
                self.disarm_capture_timeout();
                let active = {
                    let mut dialog = self.capture_dialog_state.borrow_mut();
                    apply_unsupported_capture_state(&mut dialog, state)
                };
                debug_assert!(!active);
                self.update_capture_dialog_widgets();
            }
            LayoutSwitchCapturePhase::Cancelled => {
                self.disarm_capture_timeout();
                self.reset_capture_dialog();
                self.close_layout_switch_dialog();
            }
            LayoutSwitchCapturePhase::Finished => {
                self.disarm_capture_timeout();
                self.reset_capture_dialog();
            }
        }
    }

    fn handle_capture_failed(&self, error: SettingsClientError) {
        self.disarm_capture_timeout();
        {
            let mut dialog = self.capture_dialog_state.borrow_mut();
            apply_capture_failure_state(&mut dialog);
        }
        self.update_capture_dialog_widgets();
        self.close_layout_switch_dialog();
        self.show_client_error(error, false);
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

    fn current_view_state(&self) -> ViewState {
        self.current_view_state.borrow().clone()
    }

    fn reset_capture_dialog(&self) {
        self.capture_dialog_state.borrow_mut().clear();
        self.update_capture_dialog_widgets();
    }

    fn reset_hotkey_dialog(&self, target: HotkeyDialogTarget) {
        self.hotkey_dialog_state.borrow_mut().set_target(target);
        self.update_hotkey_dialog_widgets();
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

            if !capture_timeout_should_cancel(
                generation,
                ui.capture_timeout_generation.get(),
                ui.current_view_state().layout_switch.capture_active,
            ) {
                return;
            }

            ui.cancel_capture_safely(&presenter, Some(CAPTURE_TIMEOUT_TOAST));
        });

        self.capture_timeout.borrow_mut().replace(source_id);
    }

    fn schedule_focus_loss_check(self: Rc<Self>, presenter: SettingsPresenter) {
        glib::timeout_add_local_once(CAPTURE_FOCUS_SETTLE_DELAY, move || {
            let dialog = self.layout_switch_dialog();
            if capture_focus_loss_should_cancel(
                self.current_view_state().layout_switch.capture_active,
                self.window.is_active(),
                dialog.is_active(),
            ) {
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

    fn set_hotkey_candidate(&self, hotkey: HotkeySpec) {
        let mut state = self.hotkey_dialog_state.borrow_mut();
        state.candidate = Some(hotkey);
        state.error = None;
        drop(state);
        self.update_hotkey_dialog_widgets();
    }

    fn set_hotkey_error(&self, message: impl Into<String>) {
        let mut state = self.hotkey_dialog_state.borrow_mut();
        state.candidate = None;
        state.error = Some(message.into());
        drop(state);
        self.update_hotkey_dialog_widgets();
    }

    fn set_hotkey_candidate_error(&self, hotkey: HotkeySpec, message: impl Into<String>) {
        let mut state = self.hotkey_dialog_state.borrow_mut();
        state.candidate = Some(hotkey);
        state.error = Some(message.into());
        drop(state);
        self.update_hotkey_dialog_widgets();
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

    fn update_hotkey_dialog_widgets(&self) {
        let state = self.hotkey_dialog_state.borrow().clone();
        if let Some(form) = self.form.borrow().as_ref() {
            form.selected_text_hotkey_title
                .set_subtitle(state.target.dialog_subtitle());

            match state.candidate {
                Some(hotkey) => form
                    .selected_text_hotkey_dialog_value_label
                    .set_text(&hotkey.short_label()),
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
                .set_sensitive(state.candidate.is_some() && state.error.is_none());
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

    fn manual_hotkey_row(&self) -> adw::ActionRow {
        self.form
            .borrow()
            .as_ref()
            .expect("form widgets must be installed before access")
            .manual_hotkey_row
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

    fn open_hotkey_dialog_for(&self, presenter: &SettingsPresenter, target: HotkeyDialogTarget) {
        if let Err(error) = presenter.set_hotkey_capture_inhibited(true) {
            self.show_client_error(error, false);
            return;
        }

        self.reset_hotkey_dialog(target);
        self.open_hotkey_dialog();
    }

    fn open_hotkey_dialog(&self) {
        if let Some(form) = self.form.borrow().as_ref() {
            form.selected_text_hotkey_dialog.present();
            let content_box = form.hotkey_dialog_content_box.clone();
            glib::idle_add_local_once(move || {
                content_box.grab_focus();
            });
        }
    }

    fn close_layout_switch_dialog(&self) {
        if let Some(form) = self.form.borrow().as_ref() {
            form.layout_switch_dialog.hide();
        }
    }

    fn close_hotkey_dialog(&self) {
        self.release_hotkey_capture_inhibition();
        if let Some(form) = self.form.borrow().as_ref() {
            form.selected_text_hotkey_dialog.hide();
        }
    }

    fn release_hotkey_capture_inhibition(&self) {
        let Some(presenter) = self.presenter.borrow().as_ref().cloned() else {
            return;
        };

        if let Err(error) = presenter.set_hotkey_capture_inhibited(false) {
            eprintln!("[settings] Failed to release hotkey capture inhibition: {error}");
        }
    }

    // Hotkey capture
    fn install_hotkey_capture(self: &Rc<Self>) {
        let controller = gtk::EventControllerKey::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);

        {
            let ui = Rc::clone(self);
            controller.connect_key_pressed(move |_, key, _, event_state| {
                if key == gdk::Key::Escape {
                    let target = hotkey_dialog_target(&ui.hotkey_dialog_state);
                    ui.reset_hotkey_dialog(target);
                    ui.close_hotkey_dialog();
                    return glib::Propagation::Stop;
                }

                if let Some(modifier) = hotkey_modifier_from_key(key) {
                    ui.hotkey_dialog_state
                        .borrow_mut()
                        .set_modifier(modifier, true);
                    return glib::Propagation::Stop;
                }

                if hotkey_key_is_modifier_only(key) {
                    return glib::Propagation::Stop;
                }

                if let Some(hotkey) =
                    hotkey_spec_from_dialog_state(&ui.hotkey_dialog_state, key, event_state)
                {
                    ui.set_hotkey_candidate(hotkey);
                    return glib::Propagation::Stop;
                }

                ui.set_hotkey_error(
                    "Поддерживаются F9, F10, F12, Pause, ScrollLock, Insert или Menu с модификаторами Shift, Ctrl и Alt.",
                );
                glib::Propagation::Stop
            });
        }

        {
            let ui = Rc::clone(self);
            controller.connect_key_released(move |_, key, _, _| {
                let Some(modifier) = hotkey_modifier_from_key(key) else {
                    return;
                };

                ui.hotkey_dialog_state
                    .borrow_mut()
                    .set_modifier(modifier, false);
            });
        }

        self.selected_text_hotkey_dialog()
            .add_controller(controller);
    }

    // View state rendering
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

            form.autostart_switch.set_sensitive(state.form_enabled);
            form.auto_switch_switch.set_sensitive(state.form_enabled);
            form.delay_spin.set_sensitive(state.form_enabled);
            form.fix_two_capitals_switch
                .set_sensitive(state.form_enabled);
            form.fix_accidental_caps_lock_switch
                .set_sensitive(state.form_enabled);
            form.manual_hotkey_row.set_sensitive(state.form_enabled);
            form.manual_hotkey_row.set_activatable(state.form_enabled);
            form.manual_hotkey_value_icon
                .set_visible(state.form_enabled);
            form.manual_hotkey_value_label
                .set_text(&state.manual_correction_hotkey.short_label());
            form.selected_text_hotkey_row
                .set_sensitive(state.form_enabled);
            form.selected_text_hotkey_row
                .set_activatable(state.form_enabled);
            form.selected_text_hotkey_value_icon
                .set_visible(state.form_enabled);
            form.selected_text_hotkey_value_label
                .set_text(&state.selected_text_hotkey.short_label());

            if !state.hotkey_error_text.is_empty() {
                form.manual_hotkey_row
                    .set_subtitle(&state.hotkey_error_text);
                form.selected_text_hotkey_row
                    .set_subtitle(&state.hotkey_error_text);
            } else {
                form.manual_hotkey_row.set_subtitle(
                    "Исправляет слово перед курсором или отменяет последнее переключение",
                );
                form.selected_text_hotkey_row.set_subtitle(
                    "Копирует выделение, конвертирует раскладку и вставляет текст обратно",
                );
            }

            if state.layout_prefix_warning_text.is_empty() {
                form.hotkey_warning_label.hide();
            } else {
                form.hotkey_warning_label
                    .set_text(&state.layout_prefix_warning_text);
                form.hotkey_warning_label.show();
            }

            form.layout_switch_value_label
                .set_text(&state.layout_switch.combo_label);
            form.about_session_value_label
                .set_text(format_session_type_for_about(
                    state.runtime_context.session_type,
                ));
            form.about_desktop_value_label
                .set_text(format_desktop_environment_for_about(
                    state.runtime_context.desktop_environment,
                ));
            form.about_layout_combo_value_label
                .set_text(if state.loaded {
                    &state.layout_switch.combo_label
                } else {
                    "Unknown"
                });
            form.about_layout_source_value_label
                .set_text(format_layout_switch_source_for_about(
                    state.layout_switch.source,
                    state.loaded,
                ));

            let manual_actions_enabled = state.layout_switch.editable;
            let row_is_actionable =
                state.layout_switch.actions.can_capture && manual_actions_enabled;
            form.layout_switch_value_row
                .set_activatable(row_is_actionable);
            form.layout_switch_value_row
                .set_sensitive(state.form_enabled);
            form.layout_switch_value_icon.set_visible(row_is_actionable);

            form.dialog_capture_hint.set_text(
                "Поддерживаемые варианты: Ctrl+Shift, Alt+Shift, Right Alt+Right Shift, CapsLock, Ctrl+Space, Super+Space, Left Ctrl+Left Shift, Right Ctrl+Right Shift и Left Alt+Left Shift.",
            );

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

    // Error/toast helpers
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

fn apply_unsupported_capture_state(
    dialog: &mut CaptureDialogState,
    state: LayoutSwitchCaptureState,
) -> bool {
    let message = if state.message.is_empty() {
        "Эта комбинация сейчас не поддерживается OpenSwitcher.".to_string()
    } else {
        state.message
    };
    dialog.candidate = None;
    dialog.error = Some(message);
    false
}

fn apply_capture_failure_state(dialog: &mut CaptureDialogState) -> bool {
    dialog.clear();
    false
}

fn capture_timeout_should_cancel(
    expected_generation: u64,
    current_generation: u64,
    capture_active: bool,
) -> bool {
    capture_active && expected_generation == current_generation
}

fn capture_focus_loss_should_cancel(
    capture_active: bool,
    window_active: bool,
    dialog_active: bool,
) -> bool {
    capture_active && !window_active && !dialog_active
}

// View state helpers
fn initial_view_state() -> ViewState {
    let default_settings = crate::model::Settings::default();
    ViewState {
        autostart_enabled: false,
        auto_switch_enabled: default_settings.auto_switch_enabled,
        fix_two_capitals: default_settings.fix_two_capitals,
        fix_accidental_caps_lock: default_settings.fix_accidental_caps_lock,
        layout_delay_ms: default_settings.layout_delay_ms,
        manual_correction_hotkey: default_settings.manual_correction_hotkey,
        selected_text_hotkey: default_settings.selected_text_hotkey,
        hotkey_error_text: String::new(),
        layout_prefix_warning_text: String::new(),
        runtime_context: SystemContext::default(),
        layout_switch: LayoutSwitchViewState {
            combo: default_settings.layout_switch.combo,
            combo_label: default_settings
                .layout_switch
                .combo
                .short_label()
                .to_string(),
            source: default_settings.layout_switch.source,
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

// Hotkey helpers
fn hotkey_modifier_from_key(key: gdk::Key) -> Option<HotkeyModifier> {
    match key {
        gdk::Key::Shift_L | gdk::Key::Shift_R => Some(HotkeyModifier::Shift),
        gdk::Key::Control_L | gdk::Key::Control_R => Some(HotkeyModifier::Ctrl),
        gdk::Key::Alt_L | gdk::Key::Alt_R | gdk::Key::ISO_Level3_Shift => Some(HotkeyModifier::Alt),
        _ => None,
    }
}

fn hotkey_key_is_modifier_only(key: gdk::Key) -> bool {
    matches!(
        key,
        gdk::Key::ISO_First_Group
            | gdk::Key::ISO_First_Group_Lock
            | gdk::Key::ISO_Group_Latch
            | gdk::Key::ISO_Group_Lock
            | gdk::Key::ISO_Last_Group
            | gdk::Key::ISO_Last_Group_Lock
            | gdk::Key::ISO_Level3_Latch
            | gdk::Key::ISO_Level3_Lock
            | gdk::Key::ISO_Level5_Shift
            | gdk::Key::ISO_Level5_Latch
            | gdk::Key::ISO_Level5_Lock
            | gdk::Key::ISO_Next_Group
            | gdk::Key::ISO_Next_Group_Lock
            | gdk::Key::ISO_Prev_Group
            | gdk::Key::ISO_Prev_Group_Lock
            | gdk::Key::Mode_switch
    )
}

fn hotkey_trigger_from_key(key: gdk::Key) -> Option<HotkeyTrigger> {
    match key {
        gdk::Key::F9 => Some(HotkeyTrigger::F9),
        gdk::Key::F10 => Some(HotkeyTrigger::F10),
        gdk::Key::F12 => Some(HotkeyTrigger::F12),
        gdk::Key::Pause => Some(HotkeyTrigger::Pause),
        gdk::Key::Scroll_Lock => Some(HotkeyTrigger::ScrollLock),
        gdk::Key::Insert => Some(HotkeyTrigger::Insert),
        gdk::Key::Menu => Some(HotkeyTrigger::Menu),
        _ => None,
    }
}

fn hotkey_spec_from_key_event(
    dialog_state: &HotkeyDialogState,
    key: gdk::Key,
    event_state: gdk::ModifierType,
) -> Option<HotkeySpec> {
    hotkey_trigger_from_key(key)
        .map(|trigger| hotkey_spec_from_capture_state(dialog_state, trigger, event_state))
}

fn hotkey_spec_from_dialog_state(
    dialog_state: &RefCell<HotkeyDialogState>,
    key: gdk::Key,
    event_state: gdk::ModifierType,
) -> Option<HotkeySpec> {
    let state = dialog_state.borrow();
    hotkey_spec_from_key_event(&state, key, event_state)
}

fn hotkey_dialog_target(dialog_state: &RefCell<HotkeyDialogState>) -> HotkeyDialogTarget {
    dialog_state.borrow().target
}

fn hotkey_spec_from_capture_state(
    dialog_state: &HotkeyDialogState,
    trigger: HotkeyTrigger,
    event_state: gdk::ModifierType,
) -> HotkeySpec {
    HotkeySpec::new(
        hotkey_modifiers_from_capture_state(dialog_state, event_state),
        trigger,
    )
}

fn duplicate_hotkey_dialog_error(
    target: HotkeyDialogTarget,
    candidate: HotkeySpec,
    view_state: &ViewState,
) -> Option<String> {
    match target {
        HotkeyDialogTarget::ManualCorrection
            if candidate.conflicts_exact(view_state.selected_text_hotkey) =>
        {
            Some(format!(
                "Это сочетание уже используется для выделенного текста: {}.",
                candidate.short_label()
            ))
        }
        HotkeyDialogTarget::SelectedText
            if candidate.conflicts_exact(view_state.manual_correction_hotkey) =>
        {
            Some(format!(
                "Это сочетание уже используется для ручного исправления: {}.",
                candidate.short_label()
            ))
        }
        _ => None,
    }
}

fn hotkey_modifiers_from_capture_state(
    dialog_state: &HotkeyDialogState,
    event_state: gdk::ModifierType,
) -> HotkeyModifiers {
    let tracked = dialog_state.modifiers();

    HotkeyModifiers::new(
        tracked.shift || event_state.contains(gdk::ModifierType::SHIFT_MASK),
        tracked.ctrl || event_state.contains(gdk::ModifierType::CONTROL_MASK),
        tracked.alt || event_state.contains(gdk::ModifierType::ALT_MASK),
    )
}

fn current_distro_pretty_name() -> String {
    fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|content| parse_os_release_pretty_name(&content))
        .unwrap_or_else(|| "Unknown".to_string())
}

fn parse_os_release_pretty_name(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }

        let (key, value) = line.split_once('=')?;
        if key != "PRETTY_NAME" {
            return None;
        }

        let value = unquote_os_release_value(value.trim());
        (!value.is_empty()).then_some(value)
    })
}

fn unquote_os_release_value(value: &str) -> String {
    let Some(quote) = value
        .chars()
        .next()
        .filter(|quote| *quote == '"' || *quote == '\'')
    else {
        return value.to_string();
    };

    if !value.ends_with(quote) || value.len() < 2 {
        return value.to_string();
    }

    let inner = &value[1..value.len() - 1];
    let mut result = String::with_capacity(inner.len());
    let mut escaped = false;
    for ch in inner.chars() {
        if escaped {
            result.push(ch);
            escaped = false;
        } else if quote == '"' && ch == '\\' {
            escaped = true;
        } else {
            result.push(ch);
        }
    }
    if escaped {
        result.push('\\');
    }
    result
}

fn operating_system_label() -> &'static str {
    match std::env::consts::OS {
        "linux" => "Linux",
        value => value,
    }
}

fn format_session_type_for_about(session_type: SessionType) -> &'static str {
    match session_type {
        SessionType::X11 => "X11",
        SessionType::Wayland => "Wayland",
        SessionType::Unknown => "Unknown",
    }
}

fn format_desktop_environment_for_about(desktop_environment: DesktopEnvironment) -> &'static str {
    match desktop_environment {
        DesktopEnvironment::Cinnamon => "Cinnamon",
        DesktopEnvironment::Gnome => "GNOME",
        DesktopEnvironment::Xfce => "XFCE",
        DesktopEnvironment::Kde => "KDE",
        DesktopEnvironment::Unknown => "Unknown",
    }
}

fn format_layout_switch_source_for_about(source: LayoutSwitchSource, loaded: bool) -> &'static str {
    if !loaded {
        return "Unknown";
    }

    match source {
        LayoutSwitchSource::Manual => "Manual",
        LayoutSwitchSource::AutoDetected => "AutoDetected",
        LayoutSwitchSource::AutoFallback => "AutoFallback",
        LayoutSwitchSource::Unknown => "Unknown",
    }
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

// Tests
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_timeout_is_bounded_by_daemon_absolute_lease() {
        assert!(CAPTURE_TIMEOUT < crate::daemon::capture::CAPTURE_ABSOLUTE_LEASE);
    }

    #[test]
    fn capture_timeout_only_cancels_the_current_active_session() {
        assert!(capture_timeout_should_cancel(7, 7, true));
        assert!(!capture_timeout_should_cancel(7, 8, true));
        assert!(!capture_timeout_should_cancel(7, 7, false));
    }

    #[test]
    fn capture_focus_loss_cancels_only_when_both_windows_are_inactive() {
        assert!(capture_focus_loss_should_cancel(true, false, false));
        assert!(!capture_focus_loss_should_cancel(true, true, false));
        assert!(!capture_focus_loss_should_cancel(true, false, true));
        assert!(!capture_focus_loss_should_cancel(false, false, false));
    }

    #[test]
    fn unsupported_capture_is_terminal_and_preserves_the_error() {
        let mut dialog = CaptureDialogState {
            candidate: Some(LayoutSwitchCombo::ctrl_shift()),
            error: None,
        };

        let capture_active = apply_unsupported_capture_state(
            &mut dialog,
            LayoutSwitchCaptureState::unsupported("unsupported combination"),
        );

        assert!(!capture_active);
        assert_eq!(dialog.candidate, None);
        assert_eq!(dialog.error.as_deref(), Some("unsupported combination"));
    }

    #[test]
    fn renew_failure_closes_capture_and_clears_dialog_state() {
        let mut dialog = CaptureDialogState {
            candidate: Some(LayoutSwitchCombo::alt_shift()),
            error: Some("old error".to_string()),
        };

        let capture_active = apply_capture_failure_state(&mut dialog);

        assert!(!capture_active);
        assert_eq!(dialog, CaptureDialogState::default());
    }

    #[test]
    fn hotkey_supported_triggers_are_recognized() {
        assert_eq!(
            hotkey_trigger_from_key(gdk::Key::F9),
            Some(HotkeyTrigger::F9)
        );
        assert_eq!(
            hotkey_trigger_from_key(gdk::Key::F10),
            Some(HotkeyTrigger::F10)
        );
        assert_eq!(
            hotkey_trigger_from_key(gdk::Key::F12),
            Some(HotkeyTrigger::F12)
        );
        assert_eq!(
            hotkey_trigger_from_key(gdk::Key::Pause),
            Some(HotkeyTrigger::Pause)
        );
        assert_eq!(
            hotkey_trigger_from_key(gdk::Key::Scroll_Lock),
            Some(HotkeyTrigger::ScrollLock)
        );
        assert_eq!(
            hotkey_trigger_from_key(gdk::Key::Insert),
            Some(HotkeyTrigger::Insert)
        );
        assert_eq!(
            hotkey_trigger_from_key(gdk::Key::Menu),
            Some(HotkeyTrigger::Menu)
        );
    }

    #[test]
    fn hotkey_key_helpers_reject_unsupported_keys() {
        assert_eq!(hotkey_trigger_from_key(gdk::Key::space), None);
        assert_eq!(hotkey_trigger_from_key(gdk::Key::F11), None);
        assert_eq!(hotkey_modifier_from_key(gdk::Key::Super_L), None);
        assert_eq!(hotkey_modifier_from_key(gdk::Key::space), None);
    }

    #[test]
    fn hotkey_key_helpers_treat_altgr_as_alt_modifier() {
        assert_eq!(
            hotkey_modifier_from_key(gdk::Key::ISO_Level3_Shift),
            Some(HotkeyModifier::Alt)
        );
    }

    #[test]
    fn hotkey_key_helpers_detect_unknown_modifier_keys_without_treating_space_as_modifier() {
        assert!(hotkey_key_is_modifier_only(gdk::Key::ISO_Level5_Shift));
        assert!(hotkey_key_is_modifier_only(gdk::Key::ISO_Next_Group));
        assert!(hotkey_key_is_modifier_only(gdk::Key::ISO_Prev_Group));
        assert!(hotkey_key_is_modifier_only(gdk::Key::Mode_switch));
        assert!(!hotkey_key_is_modifier_only(gdk::Key::space));
    }

    #[test]
    fn hotkey_dialog_state_accepts_zero_to_three_modifiers() {
        let bare = HotkeyDialogState::default();
        assert_eq!(bare.modifiers(), HotkeyModifiers::none());

        let mut all = HotkeyDialogState::default();
        all.shift = true;
        all.ctrl = true;
        all.alt = true;
        assert_eq!(all.modifiers(), HotkeyModifiers::shift_ctrl_alt());
    }

    #[test]
    fn hotkey_capture_uses_event_state_for_shift_ctrl_alt_f12() {
        let mut dialog_state = HotkeyDialogState::default();
        dialog_state.shift = true;
        dialog_state.ctrl = true;

        assert_eq!(
            hotkey_spec_from_key_event(&dialog_state, gdk::Key::F12, gdk::ModifierType::ALT_MASK,),
            Some(HotkeySpec::new(
                HotkeyModifiers::shift_ctrl_alt(),
                HotkeyTrigger::F12,
            )),
        );
    }

    #[test]
    fn hotkey_capture_uses_event_state_for_ctrl_alt_f12() {
        let dialog_state = HotkeyDialogState::default();

        assert_eq!(
            hotkey_spec_from_key_event(
                &dialog_state,
                gdk::Key::F12,
                gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK,
            ),
            Some(HotkeySpec::new(
                HotkeyModifiers::ctrl_alt(),
                HotkeyTrigger::F12,
            )),
        );
    }

    #[test]
    fn hotkey_capture_uses_event_state_for_shift_ctrl_alt_insert() {
        let mut dialog_state = HotkeyDialogState::default();
        dialog_state.ctrl = true;
        dialog_state.alt = true;

        assert_eq!(
            hotkey_spec_from_key_event(
                &dialog_state,
                gdk::Key::Insert,
                gdk::ModifierType::SHIFT_MASK,
            ),
            Some(HotkeySpec::new(
                HotkeyModifiers::shift_ctrl_alt(),
                HotkeyTrigger::Insert,
            )),
        );
    }

    #[test]
    fn hotkey_capture_rejects_invalid_key_without_candidate() {
        let dialog_state = HotkeyDialogState::default();

        assert_eq!(
            hotkey_spec_from_key_event(
                &dialog_state,
                gdk::Key::space,
                gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK,
            ),
            None,
        );
    }

    #[test]
    fn hotkey_capture_ignores_layout_only_keysyms() {
        let dialog_state = HotkeyDialogState::default();
        let event_state = gdk::ModifierType::SHIFT_MASK
            | gdk::ModifierType::CONTROL_MASK
            | gdk::ModifierType::ALT_MASK;

        for key in [
            gdk::Key::ISO_Next_Group,
            gdk::Key::ISO_Prev_Group,
            gdk::Key::ISO_Level5_Shift,
            gdk::Key::Mode_switch,
        ] {
            assert!(hotkey_key_is_modifier_only(key), "{key:?}");
            assert_eq!(
                hotkey_spec_from_key_event(&dialog_state, key, event_state),
                None,
                "{key:?}"
            );
        }
    }

    #[test]
    fn hotkey_dialog_state_borrow_is_released_before_candidate_update() {
        let dialog_state = RefCell::new(HotkeyDialogState::default());

        let hotkey = hotkey_spec_from_dialog_state(
            &dialog_state,
            gdk::Key::F12,
            gdk::ModifierType::SHIFT_MASK,
        )
        .expect("supported hotkey should be captured");

        dialog_state.borrow_mut().candidate = Some(hotkey);

        assert_eq!(
            dialog_state.borrow().candidate,
            Some(HotkeySpec::new(
                HotkeyModifiers::shift(),
                HotkeyTrigger::F12
            )),
        );
    }

    #[test]
    fn duplicate_hotkey_dialog_error_reports_manual_duplicate_before_accepting() {
        let duplicate = HotkeySpec::new(HotkeyModifiers::ctrl_alt(), HotkeyTrigger::F12);
        let mut view_state = initial_view_state();
        view_state.selected_text_hotkey = duplicate;

        let error = duplicate_hotkey_dialog_error(
            HotkeyDialogTarget::ManualCorrection,
            duplicate,
            &view_state,
        )
        .expect("duplicate should be reported");

        assert!(error.contains("выделенного текста"));
        assert!(error.contains("Ctrl+Alt+F12"));
    }

    #[test]
    fn duplicate_hotkey_dialog_error_reports_selected_text_duplicate_before_accepting() {
        let duplicate = HotkeySpec::new(HotkeyModifiers::shift_ctrl_alt(), HotkeyTrigger::Insert);
        let mut view_state = initial_view_state();
        view_state.manual_correction_hotkey = duplicate;

        let error =
            duplicate_hotkey_dialog_error(HotkeyDialogTarget::SelectedText, duplicate, &view_state)
                .expect("duplicate should be reported");

        assert!(error.contains("ручного исправления"));
        assert!(error.contains("Ctrl+Alt+Shift+Insert"));
    }

    #[test]
    fn duplicate_hotkey_dialog_error_allows_same_trigger_with_different_modifiers() {
        let mut view_state = initial_view_state();
        view_state.manual_correction_hotkey =
            HotkeySpec::new(HotkeyModifiers::none(), HotkeyTrigger::F12);

        assert_eq!(
            duplicate_hotkey_dialog_error(
                HotkeyDialogTarget::SelectedText,
                HotkeySpec::new(HotkeyModifiers::shift(), HotkeyTrigger::F12),
                &view_state,
            ),
            None
        );
    }

    #[test]
    fn hotkey_dialog_target_subtitle_matches_target() {
        assert_eq!(
            HotkeyDialogTarget::ManualCorrection.dialog_subtitle(),
            "Ручное исправление"
        );
        assert_eq!(
            HotkeyDialogTarget::SelectedText.dialog_subtitle(),
            "Выделенный текст"
        );
    }

    #[test]
    fn hotkey_dialog_target_borrow_is_released_before_reset() {
        let dialog_state = RefCell::new(HotkeyDialogState {
            target: HotkeyDialogTarget::ManualCorrection,
            ..HotkeyDialogState::default()
        });

        let target = hotkey_dialog_target(&dialog_state);
        dialog_state.borrow_mut().set_target(target);

        assert_eq!(
            dialog_state.borrow().target,
            HotkeyDialogTarget::ManualCorrection
        );
    }

    #[test]
    fn os_release_pretty_name_parser_reads_quoted_value() {
        let content = r#"
ID=linuxmint
PRETTY_NAME="Linux Mint 22.2"
VERSION_ID="22.2"
"#;

        assert_eq!(
            parse_os_release_pretty_name(content),
            Some("Linux Mint 22.2".to_string())
        );
    }

    #[test]
    fn os_release_pretty_name_parser_handles_unquoted_value() {
        let content = "ID=ubuntu\nPRETTY_NAME=Ubuntu 24.04.3 LTS\n";

        assert_eq!(
            parse_os_release_pretty_name(content),
            Some("Ubuntu 24.04.3 LTS".to_string())
        );
    }

    #[test]
    fn os_release_pretty_name_parser_ignores_missing_or_empty_value() {
        assert_eq!(parse_os_release_pretty_name("ID=linuxmint\n"), None);
        assert_eq!(parse_os_release_pretty_name("PRETTY_NAME=\"\"\n"), None);
    }
}
