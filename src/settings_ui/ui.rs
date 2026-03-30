use super::dbus_client::SettingsDbusClient;
use super::presenter::{PresenterEvent, SaveRequest, SettingsPresenter};
use super::state::ViewState;
use crate::error::SettingsClientError;
use adw::prelude::*;
use gtk::glib::{self, SignalHandlerId};
use std::cell::RefCell;
use std::rc::Rc;

const WINDOW_WIDTH: i32 = 460;
const WINDOW_HEIGHT: i32 = 340;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SettingsWindowMode {
    #[default]
    Embedded,
    Standalone,
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

    content.append(&group);
    clamp.set_child(Some(&content));

    FormWidgets {
        clamp,
        delay_spin,
        undo_dropdown,
        delay_handler: None,
        undo_handler: None,
    }
}

struct FormWidgets {
    clamp: adw::Clamp,
    delay_spin: gtk::SpinButton,
    undo_dropdown: gtk::DropDown,
    delay_handler: Option<SignalHandlerId>,
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

    fn set_presenter(&self, presenter: SettingsPresenter) {
        self.presenter.replace(Some(presenter));
    }

    fn reload_from_daemon(&self) {
        if let Some(presenter) = self.presenter.borrow().as_ref().cloned() {
            presenter.reload();
        }
    }

    fn discard_pending_changes(&self) {
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

    fn apply_view_state(&self, state: &ViewState) {
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
            let selected_index = crate::model::UndoKey::ALL
                .iter()
                .position(|key| *key == state.undo_key)
                .unwrap_or(0);
            form.undo_dropdown.set_selected(selected_index as u32);
            if let Some(undo_handler) = &form.undo_handler {
                form.undo_dropdown.unblock_signal(undo_handler);
            }

            form.delay_spin.set_sensitive(state.form_enabled);
            form.undo_dropdown.set_sensitive(state.form_enabled);
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
        layout_delay_ms: crate::model::Settings::default().layout_delay_ms,
        undo_key: crate::model::Settings::default().undo_key,
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
