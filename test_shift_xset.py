import os
import subprocess
import time
import evdev

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

keyboard.write(evdev.ecodes.EV_KEY, evdev.ecodes.KEY_LEFTSHIFT, 1)
keyboard.write(evdev.ecodes.EV_SYN, evdev.ecodes.SYN_REPORT, 0)
time.sleep(0.1)

# Run xset
out = subprocess.check_output(['xset', '-q'], env={'DISPLAY': ':0.0', 'XAUTHORITY': '/home/fly/.Xauthority'})
print(out.decode())

keyboard.write(evdev.ecodes.EV_KEY, evdev.ecodes.KEY_LEFTSHIFT, 0)
keyboard.write(evdev.ecodes.EV_SYN, evdev.ecodes.SYN_REPORT, 0)
time.sleep(0.1)
