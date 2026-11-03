#!/usr/bin/env python3
"""Verify user-takeover handling on the owned native fixture.

Run with PYTHONPATH=sdk/python/src. No screenshot or accessibility content is
printed. Uses button state as evidence of dispatch. Default injects a foreign-process
mouse event; --manual requires a physical mouse movement during the hold.
"""
from __future__ import annotations

import argparse
import ctypes
import threading
import subprocess
import sys
import time

from nexus_cua import (
    AccessibilityMode, ActionKind, CapabilityManifest, Client, Config, CUAError,
    ErrorCode, FocusWindow, MutationIndeterminateError, MutationStatus, NormalizedPoint, PermissionMode,
    PressPoint, select_application, select_window,
)



FOREIGN_MOVE = r"""
import ctypes
class Point(ctypes.Structure):
    _fields_ = [("x", ctypes.c_double), ("y", ctypes.c_double)]
cg = ctypes.CDLL("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics")
cg.CGEventCreate.argtypes = [ctypes.c_void_p]
cg.CGEventCreate.restype = ctypes.c_void_p
cg.CGEventGetLocation.argtypes = [ctypes.c_void_p]
cg.CGEventGetLocation.restype = Point
cg.CGEventCreateMouseEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint32, Point, ctypes.c_uint32]
cg.CGEventCreateMouseEvent.restype = ctypes.c_void_p
cg.CGEventPost.argtypes = [ctypes.c_uint32, ctypes.c_void_p]
event = cg.CGEventCreate(None)
point = cg.CGEventGetLocation(event)
point.x += 1
cg.CGEventPost(0, cg.CGEventCreateMouseEvent(None, 5, point, 0))
"""

def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--endpoint", required=True)
    parser.add_argument("--token-file", required=True)
    parser.add_argument("--manual", action="store_true")
    args = parser.parse_args()
    if args.manual:
        subprocess.run(["/usr/bin/say", "请先松开鼠标和键盘，三秒后开始测试"], check=True, timeout=8)
        time.sleep(3)
    config = Config(args.endpoint, args.token_file)
    client = Client(config)
    cg = ctypes.CDLL("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics")
    button_state = cg.CGEventSourceButtonState
    button_state.argtypes = (ctypes.c_int32, ctypes.c_uint32)
    button_state.restype = ctypes.c_bool
    if button_state(0, 0):
        raise RuntimeError("left button already held; probe must start idle")
    app = select_application(client.discover_applications().applications,
                             "dev.nexus.cua.native-fixture")
    session = client.open_session(CapabilityManifest(
        mode=PermissionMode.BOUNDED, application_refs=(app.discovery_ref,),
        allowed_actions=(ActionKind.FOCUS_WINDOW, ActionKind.PRESS_POINT),
        allow_foreground_input=True, ttl_seconds=60,
    ))
    worker = None
    closed = False
    try:
        window = select_window(client.list_windows(session.session_id), "Generation")
        guard = client.observe_window(session.session_id, window.window_ref,
                                      include_screenshot=False, accessibility=AccessibilityMode.DISABLED)
        client.perform_action(session.session_id, window.window_ref, guard.observation_id, FocusWindow())
        observed = client.observe_window(session.session_id, window.window_ref,
                                         include_screenshot=True, accessibility=AccessibilityMode.FULL)
        target = next(element for element in observed.elements
                      if element.name == "Increment Counter" and "invoke" in element.actions)
        bounds = target.screen_bounds
        mapping = observed.screenshot.mapping.screen_bounds
        point = NormalizedPoint(
            (bounds.x + bounds.width / 2 - mapping.x) / mapping.width * 1000,
            (bounds.y + bounds.height / 2 - mapping.y) / mapping.height * 1000,
        )
        outcomes = []

        def press() -> None:
            try:
                outcomes.append(Client(config).perform_action(
                    session.session_id, window.window_ref, observed.observation_id,
                    PressPoint(point, duration_ms=10000 if args.manual else 3000)))
            except Exception as error:
                outcomes.append(error)

        worker = threading.Thread(target=press, daemon=True)
        worker.start()
        deadline = time.monotonic() + 5
        while not button_state(0, 0):
            if not worker.is_alive() or time.monotonic() >= deadline:
                if outcomes:
                    failure = outcomes[0]
                    if isinstance(failure, MutationIndeterminateError):
                        failure = failure.cause
                    if isinstance(failure, CUAError):
                        print(f"pre_hold_error code={failure.code.value} mutation={failure.mutation_status.value}", flush=True)
                raise RuntimeError("no native button-down observed")
            time.sleep(0.005)
        print("takeover_probe button_down: move physical mouse now" if args.manual else "takeover_probe button_down", flush=True)
        close_started = time.monotonic()
        if args.manual:
            subprocess.run(["/usr/bin/say", "开始，请移动鼠标"], check=True, timeout=4)
        else:
            subprocess.run([sys.executable, "-c", FOREIGN_MOVE], check=True, timeout=2)
        while button_state(0, 0) and time.monotonic() - close_started < (10 if args.manual else 1):
            time.sleep(0.005)
        elapsed = time.monotonic() - close_started
        if button_state(0, 0):
            raise RuntimeError("button remained held one second after revocation")
        worker.join(timeout=2)
        if worker.is_alive() or len(outcomes) != 1:
            raise RuntimeError("revoked action did not finish within two seconds")
        outcome = outcomes[0]
        if isinstance(outcome, MutationIndeterminateError):
            outcome = outcome.cause
        if not (isinstance(outcome, CUAError)
                and outcome.code is ErrorCode.USER_ACTIVE
                and outcome.mutation_status is MutationStatus.INDETERMINATE):
            if isinstance(outcome, CUAError):
                print(f"hold_result code={outcome.code.value} mutation={outcome.mutation_status.value}", flush=True)
            else:
                print(f"hold_result type={type(outcome).__name__}", flush=True)
            raise RuntimeError("partial dispatch did not report indeterminate user takeover")
        # A new request must not regain authority even with an old observation.
        try:
            client.perform_action(session.session_id, window.window_ref, observed.observation_id, FocusWindow())
        except CUAError as error:
            if error.code is not ErrorCode.USER_ACTIVE or error.mutation_status is not MutationStatus.NOT_DISPATCHED:
                raise RuntimeError("subsequent action did not report latched user takeover") from error
        else:
            raise RuntimeError("session resumed after user takeover")
        mode = "physical" if args.manual else "foreign_synthetic"
        print(f"user_takeover passed mode={mode} elapsed_ms={elapsed * 1000:.1f}")
    finally:
        if not closed:
            client.close_session(session.session_id)
        if worker is not None:
            worker.join(timeout=5)


if __name__ == "__main__":
    main()
