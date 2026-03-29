use super::dbus_client::SettingsDbusClient;
use super::presenter::{PresenterEvent, SettingsPresenter};
use super::state::ViewState;
use crate::error::SettingsClientError;
use adw::prelude::*;
use gtk::glib::{self, SignalHandlerId};
use std::rc::Rc;

pub fn run() {
    let app = adw::Application::builder()
        .application_id("org.oswitch.settings")
        .build();

    app.connect_activate(build_ui);
    app.run();
}

fn build_ui(app: &adw::Application) {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Настройки OpenSwitcher")
        .default_width(460)
        .default_height(260)
        .build();

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

    let clamp = adw::Clamp::new();
    clamp.set_margin_top(6);
    clamp.set_margin_bottom(12);
    clamp.set_margin_start(12);
    clamp.set_margin_end(12);

    let page = adw::PreferencesPage::new();
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

    page.add(&group);
    clamp.set_child(Some(&page));
    content_box.append(&clamp);

    toast_overlay.set_child(Some(&content_box));
    toolbar.set_content(Some(&toast_overlay));

    let action_bar = gtk::ActionBar::new();
    let cancel_button = gtk::Button::with_label("Отмена");
    let save_button = gtk::Button::with_label("Сохранить");
    save_button.add_css_class("suggested-action");
    action_bar.pack_start(&cancel_button);
    action_bar.pack_end(&save_button);
    toolbar.add_bottom_bar(&action_bar);

    window.set_content(Some(&toolbar));

    let (event_tx, event_rx) = async_channel::unbounded();
    let presenter = SettingsPresenter::new(SettingsDbusClient, event_tx);

    let delay_handler = {
        let presenter = presenter.clone();
        delay_spin.connect_value_changed(move |spin| {
            presenter.update_layout_delay(spin.value_as_int() as u32);
        })
    };

    let undo_handler = {
        let presenter = presenter.clone();
        undo_dropdown.connect_selected_notify(move |dropdown| {
            if let Some(key) = crate::model::UndoKey::ALL
                .get(dropdown.selected() as usize)
                .copied()
            {
                presenter.update_undo_key(key);
            }
        })
    };

    {
        let presenter = presenter.clone();
        save_button.connect_clicked(move |_| presenter.save());
    }

    {
        let window = window.clone();
        cancel_button.connect_clicked(move |_| window.close());
    }

    let ui = Rc::new(SettingsWindow {
        window: window.clone(),
        toast_overlay,
        status_label,
        delay_spin,
        undo_dropdown,
        save_button,
        cancel_button,
        delay_handler,
        undo_handler,
    });

    ui.apply_view_state(&ViewState {
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
    });

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
                    PresenterEvent::SaveSucceeded(result) => {
                        ui.show_toast(&result.message);
                    }
                }
            }
        });
    }

    window.present();
    presenter.initialize();
}

struct SettingsWindow {
    window: adw::ApplicationWindow,
    toast_overlay: adw::ToastOverlay,
    status_label: gtk::Label,
    delay_spin: gtk::SpinButton,
    undo_dropdown: gtk::DropDown,
    save_button: gtk::Button,
    cancel_button: gtk::Button,
    delay_handler: SignalHandlerId,
    undo_handler: SignalHandlerId,
}

impl SettingsWindow {
    fn apply_view_state(&self, state: &ViewState) {
        self.delay_spin.block_signal(&self.delay_handler);
        self.delay_spin.set_value(state.layout_delay_ms as f64);
        self.delay_spin.unblock_signal(&self.delay_handler);

        self.undo_dropdown.block_signal(&self.undo_handler);
        let selected_index = crate::model::UndoKey::ALL
            .iter()
            .position(|key| *key == state.undo_key)
            .unwrap_or(0);
        self.undo_dropdown.set_selected(selected_index as u32);
        self.undo_dropdown.unblock_signal(&self.undo_handler);

        self.delay_spin.set_sensitive(state.form_enabled);
        self.undo_dropdown.set_sensitive(state.form_enabled);
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
