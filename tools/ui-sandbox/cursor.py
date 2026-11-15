"""Print the cursor image currently set on an X display, via XFixes (read-only).

Usage: python3 -I cursor.py DISPLAY
"""
import ctypes, sys
x11 = ctypes.CDLL("libX11.so.6"); xf = ctypes.CDLL("libXfixes.so.3")
class Img(ctypes.Structure):
    _fields_ = [("x", ctypes.c_short), ("y", ctypes.c_short), ("width", ctypes.c_ushort), ("height", ctypes.c_ushort),
                ("xhot", ctypes.c_ushort), ("yhot", ctypes.c_ushort), ("serial", ctypes.c_ulong),
                ("pixels", ctypes.POINTER(ctypes.c_ulong)), ("atom", ctypes.c_ulong), ("name", ctypes.c_char_p)]
x11.XOpenDisplay.restype = ctypes.c_void_p; x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
xf.XFixesGetCursorImage.restype = ctypes.POINTER(Img); xf.XFixesGetCursorImage.argtypes = [ctypes.c_void_p]
dpy = x11.XOpenDisplay(sys.argv[1].encode())
img = xf.XFixesGetCursorImage(dpy).contents
n = img.width * img.height
opaque = sum(1 for i in range(n) if (img.pixels[i] >> 24) & 0xff)
print(f"cursor name={img.name!r} size={img.width}x{img.height} at=({img.x},{img.y}) opaque_pixels={opaque}/{n}")
