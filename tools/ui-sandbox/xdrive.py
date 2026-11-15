"""Drive a nested X display (Xephyr) for UI testing: synthetic keys, clicks and
typing via XTest, screenshots via xwd, and closing windows like a window manager.

Never point it at your desktop: keystrokes would go to whatever has focus there.

Usage: python3 -I xdrive.py DISPLAY ACTION...
  focus:TITLE        give the window titled TITLE keyboard focus (no WM in Xephyr)
  key:Down  key:ctrl+F6  key:shift+Tab  type:some/text
  move:X,Y  click:X,Y  dclick:X,Y  mclick:ctrl,X,Y  button:N,X,Y
  sleep:SECONDS  shot:/path/out.png  close:TITLE

Example (see docs/development.md):
  Xephyr :5 -screen 1280x800 -no-host-grab &
  DISPLAY=:5 OMNI_CONFIG_DIR=/tmp/omni-test target/debug/omnicommander DIR DIR &
  python3 -I tools/ui-sandbox/xdrive.py :5 focus:OmniCommander key:Down shot:/tmp/a.png
"""
import ctypes
import os
import struct
import subprocess
import sys
import time

from PIL import Image

display_name = sys.argv[1]
if display_name == os.environ.get("DISPLAY") or display_name in (":0", ":1"):
    sys.exit(f"refusing to drive {display_name}: that looks like your desktop, not a sandbox")

x11 = ctypes.CDLL("libX11.so.6")
xtst = ctypes.CDLL("libXtst.so.6")
x11.XOpenDisplay.restype = ctypes.c_void_p
x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
x11.XStringToKeysym.restype = ctypes.c_ulong
x11.XStringToKeysym.argtypes = [ctypes.c_char_p]
x11.XKeysymToKeycode.restype = ctypes.c_ubyte
x11.XKeysymToKeycode.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
x11.XFlush.argtypes = [ctypes.c_void_p]
x11.XSync.argtypes = [ctypes.c_void_p, ctypes.c_int]
x11.XSetInputFocus.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_ulong]
xtst.XTestFakeKeyEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]
xtst.XTestFakeMotionEvent.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_ulong]
xtst.XTestFakeButtonEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]

dpy = x11.XOpenDisplay(display_name.encode())
if not dpy:
    sys.exit(f"cannot open display {display_name}")

MODS = {"ctrl": "Control_L", "shift": "Shift_L", "alt": "Alt_L"}


def keycode(name):
    sym = x11.XStringToKeysym(name.encode())
    if not sym:
        sys.exit(f"unknown keysym {name}")
    return x11.XKeysymToKeycode(dpy, sym)


def key(spec):
    *mods, main = spec.split("+")
    codes = [keycode(MODS[m]) for m in mods] + [keycode(main)]
    for c in codes:
        xtst.XTestFakeKeyEvent(dpy, c, 1, 0)
    for c in reversed(codes):
        xtst.XTestFakeKeyEvent(dpy, c, 0, 0)
    x11.XSync(dpy, 0)
    time.sleep(0.08)


SYMBOLS = {"*": "KP_Multiply", ";": "semicolon", "/": "slash", ".": "period", "-": "minus", " ": "space", "~": "shift+asciitilde",
           "_": "shift+underscore", ":": "shift+colon", "\\": "backslash"}


def type_text(text):
    for ch in text:
        if ch in SYMBOLS:
            key(SYMBOLS[ch])
        elif ch.isupper():
            key("shift+" + ch)
        else:
            key(ch)


def move(x, y):
    xtst.XTestFakeMotionEvent(dpy, -1, x, y, 0)
    x11.XSync(dpy, 0)


def click(x, y, times=1):
    move(x, y)
    for _ in range(times):
        xtst.XTestFakeButtonEvent(dpy, 1, 1, 0)
        xtst.XTestFakeButtonEvent(dpy, 1, 0, 0)
        x11.XSync(dpy, 0)
        time.sleep(0.05)
    time.sleep(0.1)


def shot(path):
    raw = subprocess.run(["xwd", "-root", "-silent", "-display", display_name],
                         check=True, capture_output=True).stdout
    header = struct.unpack(">25I", raw[:100])
    header_size, width, height = header[0], header[4], header[5]
    byte_order, bpp, bytes_per_line, ncolors = header[7], header[11], header[12], header[19]
    if bpp != 32:
        sys.exit(f"unsupported xwd depth {bpp}")
    offset = header_size + ncolors * 12
    data = raw[offset:offset + bytes_per_line * height]
    mode = "BGRX" if byte_order == 0 else "XRGB"
    Image.frombytes("RGB", (width, height), data, "raw", mode, bytes_per_line).save(path)


def focus(title):
    tree = subprocess.run(["xwininfo", "-root", "-tree", "-display", display_name],
                          check=True, capture_output=True, text=True).stdout
    ids = [line.split()[0] for line in tree.splitlines() if f'"{title}"' in line]
    if not ids:
        sys.exit(f"no window titled {title}")
    x11.XSetInputFocus(dpy, int(ids[0], 16), 1, 0)  # RevertToPointerRoot, CurrentTime
    x11.XSync(dpy, 0)
    time.sleep(0.2)


class XClientMessageEvent(ctypes.Structure):
    _fields_ = [("type", ctypes.c_int), ("serial", ctypes.c_ulong), ("send_event", ctypes.c_int),
                ("display", ctypes.c_void_p), ("window", ctypes.c_ulong), ("message_type", ctypes.c_ulong),
                ("format", ctypes.c_int), ("data", ctypes.c_long * 5)]


class XEvent(ctypes.Union):
    _fields_ = [("xclient", XClientMessageEvent), ("pad", ctypes.c_long * 24)]


x11.XInternAtom.restype = ctypes.c_ulong
x11.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
x11.XSendEvent.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_long, ctypes.POINTER(XEvent)]


def close(title):
    """Asks the window to close, like a window manager's close button."""
    tree = subprocess.run(["xwininfo", "-root", "-tree", "-display", display_name],
                          check=True, capture_output=True, text=True).stdout
    ids = [line.split()[0] for line in tree.splitlines() if f'"{title}"' in line]
    if not ids:
        sys.exit(f"no window titled {title}")
    win = int(ids[0], 16)
    ev = XEvent()
    ev.xclient.type = 33  # ClientMessage
    ev.xclient.window = win
    ev.xclient.message_type = x11.XInternAtom(dpy, b"WM_PROTOCOLS", 0)
    ev.xclient.format = 32
    ev.xclient.data[0] = x11.XInternAtom(dpy, b"WM_DELETE_WINDOW", 0)
    x11.XSendEvent(dpy, win, 0, 0, ctypes.byref(ev))
    x11.XSync(dpy, 0)


for action in sys.argv[2:]:
    verb, _, arg = action.partition(":")
    if verb == "key":
        key(arg)
    elif verb == "move":
        move(*map(int, arg.split(",")))
    elif verb == "click":
        click(*map(int, arg.split(",")))
    elif verb == "dclick":
        click(*map(int, arg.split(",")), times=2)
    elif verb == "button":  # button:N,X,Y
        n, x, y = map(int, arg.split(","))
        move(x, y)
        xtst.XTestFakeButtonEvent(dpy, n, 1, 0)
        xtst.XTestFakeButtonEvent(dpy, n, 0, 0)
        x11.XSync(dpy, 0)
        time.sleep(0.1)
    elif verb == "mclick":  # mclick:ctrl,X,Y  (click with a modifier held)
        mod, x, y = arg.split(",")
        code = keycode(MODS[mod])
        xtst.XTestFakeKeyEvent(dpy, code, 1, 0)
        x11.XSync(dpy, 0)
        click(int(x), int(y))
        xtst.XTestFakeKeyEvent(dpy, code, 0, 0)
        x11.XSync(dpy, 0)
        time.sleep(0.1)
    elif verb == "type":
        type_text(arg)
    elif verb == "focus":
        focus(arg)
    elif verb == "close":
        close(arg)
    elif verb == "sleep":
        time.sleep(float(arg))
    elif verb == "shot":
        shot(arg)
    else:
        sys.exit(f"unknown action {action}")
