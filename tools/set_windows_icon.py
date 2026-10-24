"""Embed a multi-size ICO in an unsigned Windows executable. Run with Windows Python."""
import ctypes
from ctypes import wintypes
from pathlib import Path
import struct
import sys


def set_icon(executable, icon):
    executable = Path(executable).resolve()
    raw = Path(icon).read_bytes()
    reserved, kind, count = struct.unpack_from("<HHH", raw)
    if reserved != 0 or kind != 1 or not count:
        raise ValueError("Expected a non-empty ICO file")
    group = bytearray(raw[:6])
    images = []
    for i in range(count):
        entry = raw[6 + i * 16:22 + i * 16]
        size, offset = struct.unpack_from("<II", entry, 8)
        payload = raw[offset:offset + size]
        if len(payload) != size:
            raise ValueError("Truncated ICO image")
        images.append(payload)
        group.extend(entry[:12] + struct.pack("<H", i + 1))
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.BeginUpdateResourceW.argtypes = [wintypes.LPCWSTR, wintypes.BOOL]
    kernel.BeginUpdateResourceW.restype = wintypes.HANDLE
    kernel.UpdateResourceW.argtypes = [wintypes.HANDLE, ctypes.c_void_p,
        ctypes.c_void_p, wintypes.WORD, ctypes.c_void_p, wintypes.DWORD]
    kernel.UpdateResourceW.restype = wintypes.BOOL
    kernel.EndUpdateResourceW.argtypes = [wintypes.HANDLE, wintypes.BOOL]
    kernel.EndUpdateResourceW.restype = wintypes.BOOL
    handle = kernel.BeginUpdateResourceW(str(executable), False)
    if not handle:
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        for resource_type, resource_id, payload in [
            *[(3, i + 1, data) for i, data in enumerate(images)],
            (14, 1, bytes(group))]:
            buffer = ctypes.create_string_buffer(payload)
            if not kernel.UpdateResourceW(handle, ctypes.c_void_p(resource_type),
                    ctypes.c_void_p(resource_id), 0, ctypes.cast(buffer, ctypes.c_void_p), len(payload)):
                raise ctypes.WinError(ctypes.get_last_error())
    except BaseException:
        kernel.EndUpdateResourceW(handle, True)
        raise
    if not kernel.EndUpdateResourceW(handle, False):
        raise ctypes.WinError(ctypes.get_last_error())
    shell = ctypes.WinDLL("shell32", use_last_error=True)
    shell.ExtractIconExW.argtypes = [wintypes.LPCWSTR, ctypes.c_int,
        ctypes.c_void_p, ctypes.c_void_p, wintypes.UINT]
    shell.ExtractIconExW.restype = wintypes.UINT
    if shell.ExtractIconExW(str(executable), -1, None, None, 0) < 1:
        raise RuntimeError("Windows could not find the embedded icon")
    print(f"Embedded and verified {count} icon sizes in {executable}")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit("Usage: python set_windows_icon.py APP.exe ICON.ico")
    set_icon(sys.argv[1], sys.argv[2])
