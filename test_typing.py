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
    print("Keyboard not found")
    exit(1)

print(f"Injecting into {keyboard.name} ({keyboard.path})")

def press(key, shift=False):
    if shift:
        keyboard.write(evdev.ecodes.EV_KEY, evdev.ecodes.KEY_LEFTSHIFT, 1)
        keyboard.write(evdev.ecodes.EV_SYN, evdev.ecodes.SYN_REPORT, 0)
        time.sleep(0.01)
    keyboard.write(evdev.ecodes.EV_KEY, key, 1)
    keyboard.write(evdev.ecodes.EV_SYN, evdev.ecodes.SYN_REPORT, 0)
    time.sleep(0.01)
    keyboard.write(evdev.ecodes.EV_KEY, key, 0)
    keyboard.write(evdev.ecodes.EV_SYN, evdev.ecodes.SYN_REPORT, 0)
    time.sleep(0.01)
    if shift:
        keyboard.write(evdev.ecodes.EV_KEY, evdev.ecodes.KEY_LEFTSHIFT, 0)
        keyboard.write(evdev.ecodes.EV_SYN, evdev.ecodes.SYN_REPORT, 0)
        time.sleep(0.01)

# Ghbdtn! 
# KEY_G (shift), KEY_H, KEY_B, KEY_D, KEY_T, KEY_N, KEY_1 (shift), KEY_SPACE
press(evdev.ecodes.KEY_G, shift=True)
press(evdev.ecodes.KEY_H)
press(evdev.ecodes.KEY_B)
press(evdev.ecodes.KEY_D)
press(evdev.ecodes.KEY_T)
press(evdev.ecodes.KEY_N)
press(evdev.ecodes.KEY_1, shift=True)
press(evdev.ecodes.KEY_SPACE)

print("Done injecting")
