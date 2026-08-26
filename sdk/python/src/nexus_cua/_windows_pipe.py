from __future__ import annotations

import ctypes
from ctypes import wintypes
import struct
import time

from ._transport import TransportError


GENERIC_READ = 0x80000000
GENERIC_WRITE = 0x40000000
OPEN_EXISTING = 3
FILE_FLAG_OVERLAPPED = 0x40000000
ERROR_IO_PENDING = 997
WAIT_OBJECT_0 = 0
WAIT_TIMEOUT = 258
INFINITE = 0xFFFFFFFF
INVALID_HANDLE_VALUE = ctypes.c_void_p(-1).value


class OVERLAPPED(ctypes.Structure):
    _fields_ = [
        ("Internal", ctypes.c_void_p),
        ("InternalHigh", ctypes.c_void_p),
        ("Offset", wintypes.DWORD),
        ("OffsetHigh", wintypes.DWORD),
        ("hEvent", wintypes.HANDLE),
    ]


kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
kernel32.WaitNamedPipeW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD]
kernel32.WaitNamedPipeW.restype = wintypes.BOOL
kernel32.CreateFileW.argtypes = [
    wintypes.LPCWSTR,
    wintypes.DWORD,
    wintypes.DWORD,
    ctypes.c_void_p,
    wintypes.DWORD,
    wintypes.DWORD,
    wintypes.HANDLE,
]
kernel32.CreateFileW.restype = wintypes.HANDLE
kernel32.CreateEventW.argtypes = [
    ctypes.c_void_p,
    wintypes.BOOL,
    wintypes.BOOL,
    wintypes.LPCWSTR,
]
kernel32.CreateEventW.restype = wintypes.HANDLE
kernel32.ReadFile.argtypes = [
    wintypes.HANDLE,
    ctypes.c_void_p,
    wintypes.DWORD,
    ctypes.POINTER(wintypes.DWORD),
    ctypes.POINTER(OVERLAPPED),
]
kernel32.ReadFile.restype = wintypes.BOOL
kernel32.WriteFile.argtypes = [
    wintypes.HANDLE,
    ctypes.c_void_p,
    wintypes.DWORD,
    ctypes.POINTER(wintypes.DWORD),
    ctypes.POINTER(OVERLAPPED),
]
kernel32.WriteFile.restype = wintypes.BOOL
kernel32.GetOverlappedResult.argtypes = [
    wintypes.HANDLE,
    ctypes.POINTER(OVERLAPPED),
    ctypes.POINTER(wintypes.DWORD),
    wintypes.BOOL,
]
kernel32.GetOverlappedResult.restype = wintypes.BOOL
kernel32.CancelIoEx.argtypes = [wintypes.HANDLE, ctypes.POINTER(OVERLAPPED)]
kernel32.CancelIoEx.restype = wintypes.BOOL
kernel32.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
kernel32.WaitForSingleObject.restype = wintypes.DWORD
kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
kernel32.CloseHandle.restype = wintypes.BOOL


def pipe_round_trip(
    endpoint: str, payload: bytes, max_frame_bytes: int, deadline: float
) -> bytes:
    try:
        wait_ms = _remaining_ms(deadline)
        if not kernel32.WaitNamedPipeW(endpoint, wait_ms):
            raise _last_error("wait for named pipe")
        handle = kernel32.CreateFileW(
            endpoint,
            GENERIC_READ | GENERIC_WRITE,
            0,
            None,
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            None,
        )
        if handle == INVALID_HANDLE_VALUE:
            raise _last_error("open named pipe")
    except (OSError, TimeoutError) as error:
        raise TransportError(str(error)) from error
    try:
        _write_all(handle, struct.pack(">I", len(payload)) + payload, deadline)
        header = _read_exact(handle, 4, deadline)
        length = struct.unpack(">I", header)[0]
        if length == 0 or length > max_frame_bytes:
            raise TransportError(
                f"response frame length {length} is outside the configured bound"
            )
        return _read_exact(handle, length, deadline)
    finally:
        kernel32.CloseHandle(handle)


def _write_all(handle: int, payload: bytes, deadline: float) -> None:
    offset = 0
    while offset < len(payload):
        buffer = ctypes.create_string_buffer(payload[offset:])
        written = _overlapped_io(
            kernel32.WriteFile, handle, buffer, len(payload) - offset, deadline
        )
        if written <= 0:
            raise TransportError("named-pipe write made no progress")
        offset += written


def _read_exact(handle: int, length: int, deadline: float) -> bytes:
    output = bytearray()
    while len(output) < length:
        size = length - len(output)
        buffer = ctypes.create_string_buffer(size)
        read = _overlapped_io(kernel32.ReadFile, handle, buffer, size, deadline)
        if read <= 0:
            raise TransportError("sidecar closed before the response frame completed")
        output.extend(buffer.raw[:read])
    return bytes(output)


def _overlapped_io(
    function: object, handle: int, buffer: object, size: int, deadline: float
) -> int:
    event = kernel32.CreateEventW(None, True, False, None)
    if not event:
        raise _last_error("create overlapped event")
    overlapped = OVERLAPPED(hEvent=event)
    transferred = wintypes.DWORD()
    try:
        completed = function(
            handle, buffer, size, ctypes.byref(transferred), ctypes.byref(overlapped)
        )
        if completed:
            return int(transferred.value)
        error = ctypes.get_last_error()
        if error != ERROR_IO_PENDING:
            raise ctypes.WinError(error)
        wait = kernel32.WaitForSingleObject(event, _remaining_ms(deadline))
        if wait == WAIT_TIMEOUT:
            kernel32.CancelIoEx(handle, ctypes.byref(overlapped))
            kernel32.WaitForSingleObject(event, INFINITE)
            raise TimeoutError("named-pipe wait deadline elapsed")
        if wait != WAIT_OBJECT_0:
            raise _last_error("wait for named-pipe operation")
        if not kernel32.GetOverlappedResult(
            handle, ctypes.byref(overlapped), ctypes.byref(transferred), False
        ):
            raise _last_error("finish named-pipe operation")
        return int(transferred.value)
    except (OSError, TimeoutError) as error:
        raise TransportError(str(error)) from error
    finally:
        kernel32.CloseHandle(event)


def _remaining_ms(deadline: float) -> int:
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise TimeoutError("named-pipe wait deadline elapsed")
    return max(1, min(int(remaining * 1000), 0xFFFFFFFE))


def _last_error(operation: str) -> OSError:
    error = ctypes.get_last_error()
    return OSError(error, f"{operation}: {ctypes.FormatError(error)}")
