import evdev
import time

devices = [evdev.InputDevice(path) for path in evdev.list_devices()]
keyboard = None
for dev in devices:
    if 'Virtual' not in dev.name and 'Button' not in dev.name and 'Camera' not in dev.name:
        cap = dev.capabilities()
        if evdev.ecodes.EV_KEY in cap and evdev.ecodes.KEY_ENTER in cap[evdev.ecodes.EV_KEY]:
            keyboard = dev
            break

if not keyboard:
    exit(1)

keyboard.write(evdev.ecodes.EV_KEY, evdev.ecodes.KEY_LEFTCTRL, 1)
keyboard.write(evdev.ecodes.EV_KEY, evdev.ecodes.KEY_LEFTSHIFT, 1)
keyboard.write(evdev.ecodes.EV_SYN, evdev.ecodes.SYN_REPORT, 0)
time.sleep(0.02)
keyboard.write(evdev.ecodes.EV_KEY, evdev.ecodes.KEY_LEFTSHIFT, 0)
keyboard.write(evdev.ecodes.EV_KEY, evdev.ecodes.KEY_LEFTCTRL, 0)
keyboard.write(evdev.ecodes.EV_SYN, evdev.ecodes.SYN_REPORT, 0)
time.sleep(0.1)
