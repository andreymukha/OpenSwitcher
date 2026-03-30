use super::dbus_client::SettingsDbusClient;
use super::presenter::{PresenterEvent, SaveRequest, SettingsPresenter};
use super::state::{LayoutSwitchActionsState, LayoutSwitchViewState, ViewState};
use crate::error::SettingsClientError;
use crate::model::{LayoutModifier, LayoutSwitchCombo, LayoutTriggerKey};
use adw::prelude::*;
use gtk::gdk;
use gtk::glib::{self, SignalHandlerId};
use std::cell::RefCell;
use std::rc::Rc;

const WINDOW_WIDTH: i32 = 520;
const WINDOW_HEIGHT: i32 = 460;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SettingsWindowMode {
    #[default]
    Embedded,
    Standalone,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CaptureProgress {
    ctrl: bool,
    alt: bool,
    shift: bool,
    super_key: bool,
}

impl CaptureProgress {
    fn set_modifier(&mut self, modifier: LayoutModifier, pressed: bool) {
        match modifier {
            LayoutModifier::Ctrl => self.ctrl = pressed,
            LayoutModifier::Alt => self.alt = pressed,
            LayoutModifier::Shift => self.shift = pressed,
            LayoutModifier::Super => self.super_key = pressed,
        }
    }

    fn combo_with_key(self, key: LayoutTriggerKey) -> Option<LayoutSwitchCombo> {
        LayoutSwitchCombo::from_parts(self.ctrl, self.alt, self.shift, self.super_key, Some(key))
            .ok()
    }

    fn modifier_only_combo(self) -> Option<LayoutSwitchCombo> {
        LayoutSwitchCombo::from_parts(self.ctrl, self.alt, self.shift, self.super_key, None).ok()
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
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
    let form = build_form_widgets();
    ui.install_form(form);

    let (event_tx, event_rx) = async_channel::unbounded();
    let presenter = SettingsPresenter::new(SettingsDbusClient, event_tx);
    ui.set_presenter(presenter.clone());
    ui.install_capture_controller(presenter.clone());

    let delay_handler = {
        let presenter = presenter.clone();
        let delay_spin = ui.delay_spin();
        delay_spin.connect_value_changed(move |spin| {
            presenter.update_layout_delay(spin.value_as_int() as u32);
        })
    };
    ui.set_delay_handler(delay_handler);

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

    let layout_switch_handler = {
        let presenter = presenter.clone();
        let dropdown = ui.layout_switch_dropdown();
        dropdown.connect_selected_notify(move |dropdown| {
            if let Some(combo) = crate::model::LayoutSwitchCombo::COMMON_CHOICES
                .get(dropdown.selected() as usize)
                .copied()
            {
                presenter.update_layout_switch_combo(combo);
            }
        })
    };
    ui.set_layout_switch_handler(layout_switch_handler);

    {
        let presenter = presenter.clone();
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
        ui.capture_button().connect_clicked(move |_| {
            if ui.current_view_state().layout_switch.capture_active {
                presenter.cancel_layout_switch_capture();
            } else {
                presenter.start_layout_switch_capture();
            }
        });
    }

    {
        let presenter = presenter.clone();
        ui.choose_presets_button().connect_clicked(move |_| {
            presenter.show_layout_switch_presets();
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
        glib::MainContext::default().spawn_local(async move {
            while let Ok(event) = event_rx.recv().await {
                match event {
                    PresenterEvent::ViewStateChanged(view_state) => {
                        ui.apply_view_state(&view_state)
                    }
                    PresenterEvent::LoadFailed(error) => ui.show_client_error(error, true),
                    PresenterEvent::SaveFailed(error) => ui.show_client_error(error, false),
                    PresenterEvent::SaveSucceeded(result) => ui.show_toast(&result.message),
                }
            }
        });
    }

    presenter.initialize();
}

fn build_form_widgets() -> FormWidgets {
    let clamp = adw::Clamp::new();
    clamp.set_margin_top(8);
    clamp.set_margin_bottom(12);
    clamp.set_margin_start(12);
    clamp.set_margin_end(12);
    clamp.set_maximum_size(WINDOW_WIDTH);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let group = adw::PreferencesGroup::builder()
        .title("Основные настройки")
        .description("Настройки получает и сохраняет только демон через D-Bus.")
        .build();

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
    group.add(&delay_row);

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
    group.add(&undo_row);

    let layout_switch_value_row = adw::ActionRow::builder()
        .title("Комбинация переключения раскладки")
        .subtitle("Автоматически определяется демоном и используется для возврата раскладки назад")
        .build();
    let layout_switch_value_label = gtk::Label::new(Some("Ctrl+Shift"));
    layout_switch_value_label.set_halign(gtk::Align::End);
    layout_switch_value_label.set_valign(gtk::Align::Center);
    layout_switch_value_label.add_css_class("monospace");
    layout_switch_value_row.add_suffix(&layout_switch_value_label);
    group.add(&layout_switch_value_row);

    let layout_switch_actions_row = adw::ActionRow::builder()
        .title("Ручной выбор")
        .subtitle(
            "Основной путь: захватить реальную комбинацию. Список оставлен как запасной вариант.",
        )
        .build();
    let actions_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let capture_button = gtk::Button::with_label("Нажмите комбинацию");
    let choose_presets_button = gtk::Button::with_label("Выбрать из списка");
    actions_box.append(&capture_button);
    actions_box.append(&choose_presets_button);
    layout_switch_actions_row.add_suffix(&actions_box);
    group.add(&layout_switch_actions_row);

    let layout_switch_presets_revealer = gtk::Revealer::new();
    layout_switch_presets_revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
    layout_switch_presets_revealer.set_reveal_child(false);

    let layout_switch_presets_row = adw::ActionRow::builder()
        .title("Выбор из списка")
        .subtitle("Запасной вариант, если текущую комбинацию неудобно захватывать прямо сейчас")
        .build();
    let layout_switch_labels: Vec<String> = crate::model::LayoutSwitchCombo::COMMON_CHOICES
        .iter()
        .map(|combo| combo.short_label())
        .collect();
    let layout_switch_label_refs: Vec<&str> =
        layout_switch_labels.iter().map(String::as_str).collect();
    let layout_switch_dropdown = gtk::DropDown::from_strings(&layout_switch_label_refs);
    layout_switch_dropdown.set_valign(gtk::Align::Center);
    layout_switch_presets_row.add_suffix(&layout_switch_dropdown);
    layout_switch_presets_row.set_activatable_widget(Some(&layout_switch_dropdown));
    layout_switch_presets_revealer.set_child(Some(&layout_switch_presets_row));
    group.add(&layout_switch_presets_revealer);

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
    group.add(&layout_switch_hint_label);

    content.append(&group);
    clamp.set_child(Some(&content));

    FormWidgets {
        clamp,
        delay_spin,
        undo_dropdown,
        layout_switch_value_label,
        capture_button,
        choose_presets_button,
        layout_switch_presets_revealer,
        layout_switch_dropdown,
        layout_switch_hint_label,
        delay_handler: None,
        undo_handler: None,
        layout_switch_handler: None,
    }
}

struct FormWidgets {
    clamp: adw::Clamp,
    delay_spin: gtk::SpinButton,
    undo_dropdown: gtk::DropDown,
    layout_switch_value_label: gtk::Label,
    capture_button: gtk::Button,
    choose_presets_button: gtk::Button,
    layout_switch_presets_revealer: gtk::Revealer,
    layout_switch_dropdown: gtk::DropDown,
    layout_switch_hint_label: gtk::Label,
    delay_handler: Option<SignalHandlerId>,
    undo_handler: Option<SignalHandlerId>,
    layout_switch_handler: Option<SignalHandlerId>,
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
    capture_progress: RefCell<CaptureProgress>,
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
        let title = adw::WindowTitle::new("Настройки OpenSwitcher", "Общие");
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
            capture_progress: RefCell::new(CaptureProgress::default()),
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
        self.form_container.append(&form.clamp);
        self.form.replace(Some(form));
        self.apply_view_state(&initial_view_state());
    }

    fn install_capture_controller(self: &Rc<Self>, presenter: SettingsPresenter) {
        let controller = gtk::EventControllerKey::new();

        {
            let ui_weak = Rc::downgrade(self);
            let presenter = presenter.clone();
            controller.connect_key_pressed(move |_, key, _, _| {
                let Some(ui) = ui_weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };

                if !ui.current_view_state().layout_switch.capture_active {
                    return glib::Propagation::Proceed;
                }

                if key == gdk::Key::Escape {
                    ui.reset_capture_progress();
                    presenter.cancel_layout_switch_capture();
                    return glib::Propagation::Stop;
                }

                if let Some(modifier) = layout_modifier_from_key(key) {
                    ui.capture_progress.borrow_mut().set_modifier(modifier, true);
                    return glib::Propagation::Stop;
                }

                if let Some(trigger_key) = layout_trigger_key_from_key(key) {
                    let combo = {
                        let progress = *ui.capture_progress.borrow();
                        progress.combo_with_key(trigger_key)
                    };
                    if let Some(combo) = combo {
                        ui.reset_capture_progress();
                        presenter.apply_captured_layout_switch(combo);
                    } else {
                        ui.show_toast(
                            "Эта комбинация пока не поддерживается. Попробуйте Ctrl+Shift, Alt+Shift, CapsLock, Ctrl+Space или Super+Space.",
                        );
                    }
                    return glib::Propagation::Stop;
                }

                ui.show_toast(
                    "Сейчас поддерживаются Ctrl+Shift, Alt+Shift, CapsLock, Ctrl+Space и Super+Space.",
                );
                glib::Propagation::Stop
            });
        }

        {
            let ui_weak = Rc::downgrade(self);
            controller.connect_key_released(move |_, key, _, _| {
                let Some(ui) = ui_weak.upgrade() else {
                    return;
                };

                if !ui.current_view_state().layout_switch.capture_active {
                    return;
                }

                let Some(modifier) = layout_modifier_from_key(key) else {
                    return;
                };

                let should_finalize = {
                    let progress = *ui.capture_progress.borrow();
                    progress
                        .modifier_only_combo()
                        .is_some_and(|combo| combo.modifiers_count() >= 2)
                };

                if should_finalize {
                    if let Some(presenter) = ui.presenter.borrow().as_ref().cloned() {
                        let combo = {
                            let progress = *ui.capture_progress.borrow();
                            progress.modifier_only_combo()
                        };
                        if let Some(combo) = combo {
                            ui.reset_capture_progress();
                            presenter.apply_captured_layout_switch(combo);
                            return;
                        }
                    }
                }

                ui.capture_progress
                    .borrow_mut()
                    .set_modifier(modifier, false);
            });
        }

        self.window.add_controller(controller);
    }

    fn set_presenter(&self, presenter: SettingsPresenter) {
        self.presenter.replace(Some(presenter));
    }

    fn reload_from_daemon(&self) {
        self.reset_capture_progress();
        if let Some(presenter) = self.presenter.borrow().as_ref().cloned() {
            presenter.reload();
        }
    }

    fn discard_pending_changes(&self) {
        self.reset_capture_progress();
        if let Some(presenter) = self.presenter.borrow().as_ref().cloned() {
            presenter.discard_changes();
        }
    }

    fn set_delay_handler(&self, handler: SignalHandlerId) {
        if let Some(form) = self.form.borrow_mut().as_mut() {
            form.delay_handler = Some(handler);
        }
    }

    fn set_undo_handler(&self, handler: SignalHandlerId) {
        if let Some(form) = self.form.borrow_mut().as_mut() {
            form.undo_handler = Some(handler);
        }
    }

    fn set_layout_switch_handler(&self, handler: SignalHandlerId) {
        if let Some(form) = self.form.borrow_mut().as_mut() {
            form.layout_switch_handler = Some(handler);
        }
    }

    fn current_view_state(&self) -> ViewState {
        self.current_view_state.borrow().clone()
    }

    fn reset_capture_progress(&self) {
        self.capture_progress.borrow_mut().clear();
    }

    fn delay_spin(&self) -> gtk::SpinButton {
        self.form
            .borrow()
            .as_ref()
            .expect("form widgets must be installed before access")
            .delay_spin
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

    fn capture_button(&self) -> gtk::Button {
        self.form
            .borrow()
            .as_ref()
            .expect("form widgets must be installed before access")
            .capture_button
            .clone()
    }

    fn choose_presets_button(&self) -> gtk::Button {
        self.form
            .borrow()
            .as_ref()
            .expect("form widgets must be installed before access")
            .choose_presets_button
            .clone()
    }

    fn layout_switch_dropdown(&self) -> gtk::DropDown {
        self.form
            .borrow()
            .as_ref()
            .expect("form widgets must be installed before access")
            .layout_switch_dropdown
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

    fn apply_view_state(&self, state: &ViewState) {
        *self.current_view_state.borrow_mut() = state.clone();
        if !state.layout_switch.capture_active {
            self.reset_capture_progress();
        }

        if let Some(form) = self.form.borrow().as_ref() {
            if let Some(delay_handler) = &form.delay_handler {
                form.delay_spin.block_signal(delay_handler);
            }
            form.delay_spin.set_value(state.layout_delay_ms as f64);
            if let Some(delay_handler) = &form.delay_handler {
                form.delay_spin.unblock_signal(delay_handler);
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

            form.delay_spin.set_sensitive(state.form_enabled);
            form.undo_dropdown.set_sensitive(state.form_enabled);

            form.layout_switch_value_label
                .set_text(&state.layout_switch.combo_label);

            if let Some(layout_switch_handler) = &form.layout_switch_handler {
                form.layout_switch_dropdown
                    .block_signal(layout_switch_handler);
            }
            let preset_index = crate::model::LayoutSwitchCombo::COMMON_CHOICES
                .iter()
                .position(|combo| *combo == state.layout_switch.combo)
                .unwrap_or(0);
            form.layout_switch_dropdown
                .set_selected(preset_index as u32);
            if let Some(layout_switch_handler) = &form.layout_switch_handler {
                form.layout_switch_dropdown
                    .unblock_signal(layout_switch_handler);
            }

            let manual_actions_enabled = state.layout_switch.editable;
            form.capture_button.set_sensitive(
                state.layout_switch.actions.can_capture
                    && (manual_actions_enabled || state.layout_switch.capture_active),
            );
            form.capture_button
                .set_label(if state.layout_switch.capture_active {
                    "Отменить ввод"
                } else {
                    "Нажмите комбинацию"
                });
            form.choose_presets_button.set_sensitive(
                state.layout_switch.actions.can_choose_manually
                    && manual_actions_enabled
                    && !state.layout_switch.capture_active,
            );
            form.layout_switch_presets_revealer
                .set_reveal_child(state.layout_switch.show_manual_presets);
            form.layout_switch_dropdown.set_sensitive(
                state.layout_switch.show_manual_presets
                    && manual_actions_enabled
                    && !state.layout_switch.capture_active,
            );

            if state.layout_switch.capture_active {
                form.layout_switch_hint_label
                    .set_text(state.layout_switch.capture_hint);
                form.layout_switch_hint_label.show();
            } else if state.layout_switch.show_unlock_hint {
                form.layout_switch_hint_label.set_markup(
                    "Раскладка определена автоматически. Если мы определили неправильно, нажмите <a href=\"app://unlock-layout-switch\">сюда</a>.",
                );
                form.layout_switch_hint_label.show();
            } else if state.layout_switch.show_fallback_hint {
                form.layout_switch_hint_label
                    .set_text("Автоопределение раскладки не удалось. Захватите комбинацию вручную или используйте список как запасной вариант.");
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

fn layout_modifier_from_key(key: gdk::Key) -> Option<LayoutModifier> {
    match key {
        gdk::Key::Control_L | gdk::Key::Control_R => Some(LayoutModifier::Ctrl),
        gdk::Key::Alt_L | gdk::Key::Alt_R | gdk::Key::Meta_L | gdk::Key::Meta_R => {
            Some(LayoutModifier::Alt)
        }
        gdk::Key::Shift_L | gdk::Key::Shift_R => Some(LayoutModifier::Shift),
        gdk::Key::Super_L | gdk::Key::Super_R | gdk::Key::Hyper_L | gdk::Key::Hyper_R => {
            Some(LayoutModifier::Super)
        }
        _ => None,
    }
}

fn layout_trigger_key_from_key(key: gdk::Key) -> Option<LayoutTriggerKey> {
    match key {
        gdk::Key::space => Some(LayoutTriggerKey::Space),
        gdk::Key::Caps_Lock => Some(LayoutTriggerKey::CapsLock),
        _ => None,
    }
}

fn initial_view_state() -> ViewState {
    ViewState {
        layout_delay_ms: crate::model::Settings::default().layout_delay_ms,
        undo_key: crate::model::Settings::default().undo_key,
        layout_switch: LayoutSwitchViewState {
            combo: crate::model::Settings::default().layout_switch.combo,
            combo_label: crate::model::Settings::default()
                .layout_switch
                .combo
                .short_label(),
            source: crate::model::Settings::default().layout_switch.source,
            editable: false,
            manual_override_active: false,
            show_unlock_hint: false,
            show_fallback_hint: false,
            capture_active: false,
            capture_hint: "",
            show_manual_presets: false,
            actions: LayoutSwitchActionsState {
                can_auto_detect: false,
                can_capture: false,
                can_choose_manually: false,
            },
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
        SettingsClientError::Validation(error) => ("Некорректные значения", error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_progress_builds_modifier_only_combo() {
        let mut progress = CaptureProgress::default();
        progress.set_modifier(LayoutModifier::Ctrl, true);
        progress.set_modifier(LayoutModifier::Shift, true);

        assert_eq!(
            progress.modifier_only_combo(),
            Some(LayoutSwitchCombo::ctrl_shift())
        );
    }

    #[test]
    fn capture_progress_builds_combo_with_trigger_key() {
        let mut progress = CaptureProgress::default();
        progress.set_modifier(LayoutModifier::Ctrl, true);

        assert_eq!(
            progress.combo_with_key(LayoutTriggerKey::Space),
            Some(LayoutSwitchCombo::ctrl_space())
        );
        assert_eq!(
            CaptureProgress::default().combo_with_key(LayoutTriggerKey::CapsLock),
            Some(LayoutSwitchCombo::caps_lock())
        );
    }

    #[test]
    fn capture_progress_rejects_unsupported_space_without_modifiers() {
        assert_eq!(
            CaptureProgress::default().combo_with_key(LayoutTriggerKey::Space),
            None
        );
    }
}
