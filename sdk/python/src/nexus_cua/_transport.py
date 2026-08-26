from __future__ import annotations

import os
import socket
import struct
import time


class TransportError(Exception):
    pass


def round_trip(
    endpoint: str, payload: bytes, max_frame_bytes: int, deadline: float
) -> bytes:
    if not payload or len(payload) > max_frame_bytes:
        raise TransportError("request frame is outside the configured bound")
    if os.name == "nt":
        from ._windows_pipe import pipe_round_trip

        return pipe_round_trip(endpoint, payload, max_frame_bytes, deadline)
    return _unix_round_trip(endpoint, payload, max_frame_bytes, deadline)


def _unix_round_trip(
    endpoint: str, payload: bytes, max_frame_bytes: int, deadline: float
) -> bytes:
    connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        connection.settimeout(_remaining(deadline))
        connection.connect(endpoint)
        connection.settimeout(_remaining(deadline))
        connection.sendall(struct.pack(">I", len(payload)) + payload)
        header = _receive_exact(connection, 4, deadline)
        length = _response_length(header, max_frame_bytes)
        return _receive_exact(connection, length, deadline)
    except (OSError, TimeoutError) as error:
        raise TransportError(str(error)) from error
    finally:
        connection.close()


def _receive_exact(connection: socket.socket, length: int, deadline: float) -> bytes:
    output = bytearray()
    while len(output) < length:
        connection.settimeout(_remaining(deadline))
        chunk = connection.recv(length - len(output))
        if not chunk:
            raise TransportError("sidecar closed before the response frame completed")
        output.extend(chunk)
    return bytes(output)


def _remaining(deadline: float) -> float:
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise TimeoutError("sidecar wait deadline elapsed")
    return remaining


def _response_length(header: bytes, max_frame_bytes: int) -> int:
    if len(header) != 4:
        raise TransportError("response frame header must contain four bytes")
    length = struct.unpack(">I", header)[0]
    if length == 0 or length > max_frame_bytes:
        raise TransportError(
            f"response frame length {length} is outside the configured bound"
        )
    return length
