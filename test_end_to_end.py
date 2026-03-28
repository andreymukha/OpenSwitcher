import evdev
import time
import threading

# Find physical keyboard
devices = [evdev.InputDevice(path) for path in evdev.list_devices()]
real_kb = None
virt_kb = None

for dev in devices:
    if 'Virtual' in dev.name and 'Open-Switcher' in dev.name:
        virt_kb = dev
    elif 'Virtual' not in dev.name and 'Button' not in dev.name and 'Camera' not in dev.name:
        cap = dev.capabilities()
        if evdev.ecodes.EV_KEY in cap and evdev.ecodes.KEY_ENTER in cap[evdev.ecodes.EV_KEY]:
            real_kb = dev

if not real_kb or not virt_kb:
    print("Keyboards not found")
    exit(1)

print(f"Injecting into {real_kb.name}")
print(f"Listening to {virt_kb.name}")

output_keys = []

def listen():
    try:
        for event in virt_kb.read_loop():
            if event.type == evdev.ecodes.EV_KEY and event.value == 1:
                output_keys.append(event.code)
    except Exception as e:
        pass

t = threading.Thread(target=listen)
t.daemon = True
t.start()

def press(key, shift=False):
    if shift:
        real_kb.write(evdev.ecodes.EV_KEY, evdev.ecodes.KEY_LEFTSHIFT, 1)
        real_kb.write(evdev.ecodes.EV_SYN, evdev.ecodes.SYN_REPORT, 0)
        time.sleep(0.01)
    real_kb.write(evdev.ecodes.EV_KEY, key, 1)
    real_kb.write(evdev.ecodes.EV_SYN, evdev.ecodes.SYN_REPORT, 0)
    time.sleep(0.01)
    real_kb.write(evdev.ecodes.EV_KEY, key, 0)
    real_kb.write(evdev.ecodes.EV_SYN, evdev.ecodes.SYN_REPORT, 0)
    time.sleep(0.01)
    if shift:
        real_kb.write(evdev.ecodes.EV_KEY, evdev.ecodes.KEY_LEFTSHIFT, 0)
        real_kb.write(evdev.ecodes.EV_SYN, evdev.ecodes.SYN_REPORT, 0)
        time.sleep(0.01)

# Ensure layout is English for test
real_kb.write(evdev.ecodes.EV_KEY, evdev.ecodes.KEY_LEFTCTRL, 1)
real_kb.write(evdev.ecodes.EV_KEY, evdev.ecodes.KEY_LEFTSHIFT, 1)
real_kb.write(evdev.ecodes.EV_SYN, evdev.ecodes.SYN_REPORT, 0)
time.sleep(0.05)
real_kb.write(evdev.ecodes.EV_KEY, evdev.ecodes.KEY_LEFTSHIFT, 0)
real_kb.write(evdev.ecodes.EV_KEY, evdev.ecodes.KEY_LEFTCTRL, 0)
real_kb.write(evdev.ecodes.EV_SYN, evdev.ecodes.SYN_REPORT, 0)
time.sleep(0.1)

output_keys.clear()

# Inject Ghbdtn! 
press(evdev.ecodes.KEY_G, shift=True)
press(evdev.ecodes.KEY_H)
press(evdev.ecodes.KEY_B)
press(evdev.ecodes.KEY_D)
press(evdev.ecodes.KEY_T)
press(evdev.ecodes.KEY_N)
press(evdev.ecodes.KEY_1, shift=True)
press(evdev.ecodes.KEY_SPACE)

time.sleep(1) # wait for open-switcher to process and output

key_names = [evdev.ecodes.KEY[k] for k in output_keys]
print("Virtual keyboard produced:", key_names)
