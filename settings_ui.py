import sys
import os
import gi

gi.require_version("Gtk", "3.0")
from gi.repository import Gtk, Gdk

if len(sys.argv) < 2:
    print("Usage: python3 settings_ui.py <path_to_config.toml>")
    sys.exit(1)

config_path = sys.argv[1]

# Simple TOML parser/writer for our specific format to avoid adding 'toml' pip dependency
config_data = {}
with open(config_path, "r") as f:
    current_section = None
    for line in f:
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("[") and line.endswith("]"):
            current_section = line[1:-1]
            config_data[current_section] = {}
        elif "=" in line and current_section:
            key, val = line.split("=", 1)
            config_data[current_section][key.strip()] = val.strip().strip('"')

class SettingsWindow(Gtk.Window):
    def __init__(self):
        super().__init__(title="Настройки Open-Switcher")
        self.set_border_width(15)
        self.set_default_size(350, 250)
        self.set_position(Gtk.WindowPosition.CENTER)

        vbox = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=10)
        self.add(vbox)

        # Header
        label = Gtk.Label()
        label.set_markup("<b>Настройки переключения</b>")
        vbox.pack_start(label, False, False, 5)

        grid = Gtk.Grid()
        grid.set_row_spacing(10)
        grid.set_column_spacing(10)
        vbox.pack_start(grid, True, True, 0)

        # --- Layout Delay ---
        lbl1 = Gtk.Label(label="Задержка переключения раскладки (мс):")
        lbl1.set_halign(Gtk.Align.START)
        grid.attach(lbl1, 0, 0, 1, 1)

        self.delay_spin = Gtk.SpinButton.new_with_range(0, 500, 10)
        self.delay_spin.set_value(int(config_data.get("layout", {}).get("delay_ms", 30)))
        grid.attach(self.delay_spin, 1, 0, 1, 1)

        # --- Undo Key ---
        lbl2 = Gtk.Label(label="Клавиша ручного исправления:")
        lbl2.set_halign(Gtk.Align.START)
        grid.attach(lbl2, 0, 1, 1, 1)

        self.undo_combo = Gtk.ComboBoxText()
        for key in ["Pause", "F12", "ScrollLock"]:
            self.undo_combo.append_text(key)
        
        current_undo = config_data.get("features", {}).get("undo_key", "Pause")
        
        model = self.undo_combo.get_model()
        for i, row in enumerate(model):
            if row[0] == current_undo:
                self.undo_combo.set_active(i)
                break
        if self.undo_combo.get_active() == -1:
            self.undo_combo.set_active(0)

        grid.attach(self.undo_combo, 1, 1, 1, 1)

        # Buttons
        hbox = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=10)
        hbox.set_halign(Gtk.Align.END)
        vbox.pack_start(hbox, False, False, 10)

        btn_cancel = Gtk.Button(label="Отмена")
        btn_cancel.connect("clicked", self.on_cancel_clicked)
        hbox.pack_start(btn_cancel, False, False, 0)

        btn_save = Gtk.Button(label="Сохранить")
        btn_save.get_style_context().add_class("suggested-action") # Make it blue
        btn_save.connect("clicked", self.on_save_clicked)
        hbox.pack_start(btn_save, False, False, 0)

    def on_cancel_clicked(self, widget):
        Gtk.main_quit()

    def on_save_clicked(self, widget):
        # Update config string
        delay_ms = int(self.delay_spin.get_value())
        undo_key = self.undo_combo.get_active_text()

        new_config = f"""[layout]
keys = ["LeftControl", "LeftShift"]
delay_ms = {delay_ms}

[delays]
backspace_ms = 0
typing_ms = 0

[features]
undo_key = "{undo_key}"
"""
        with open(config_path, "w") as f:
            f.write(new_config)
        
        # Show success dialog
        dialog = Gtk.MessageDialog(
            transient_for=self,
            flags=0,
            message_type=Gtk.MessageType.INFO,
            buttons=Gtk.ButtonsType.OK,
            text="Настройки сохранены!",
        )
        dialog.format_secondary_text("Пожалуйста, перезапустите программу (manage.sh stop & start), чтобы изменения вступили в силу.")
        dialog.run()
        dialog.destroy()
        Gtk.main_quit()

win = SettingsWindow()
win.connect("destroy", Gtk.main_quit)
win.show_all()
Gtk.main()
