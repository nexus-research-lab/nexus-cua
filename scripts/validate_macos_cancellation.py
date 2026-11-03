#!/usr/bin/env python3
"""Verify session revocation during real input to the owned native fixture.

Run with PYTHONPATH=sdk/python/src. No screenshot or accessibility content is
printed. Uses system button state as evidence of dispatch before closing.
"""
from __future__ import annotations

import argparse
import ctypes
import threading
import time

from nexus_cua import (
    AccessibilityMode, ActionKind, CapabilityManifest, Client, Config, CUAError,
    ErrorCode, FocusWindow, MutationIndeterminateError, MutationStatus, NormalizedPoint, PermissionMode,
    PressPoint, select_application, select_window,
)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--endpoint", required=True)
    parser.add_argument("--token-file", required=True)
    args = parser.parse_args()
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
                    PressPoint(point, duration_ms=3000)))
            except Exception as error:
                outcomes.append(error)

        worker = threading.Thread(target=press, daemon=True)
        worker.start()
        deadline = time.monotonic() + 5
        while not button_state(0, 0):
            if not worker.is_alive() or time.monotonic() >= deadline:
                raise RuntimeError("no native button-down observed")
            time.sleep(0.005)
        close_started = time.monotonic()
        client.close_session(session.session_id)
        closed = True
        while button_state(0, 0) and time.monotonic() - close_started < 1:
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
                and outcome.code is ErrorCode.SESSION_UNAVAILABLE
                and outcome.mutation_status is MutationStatus.INDETERMINATE):
            raise RuntimeError("partial dispatch did not report indeterminate session revocation")
        print(f"session_close_during_press passed release_ms={elapsed * 1000:.1f}")
    finally:
        if not closed:
            client.close_session(session.session_id)
        if worker is not None:
            worker.join(timeout=5)


if __name__ == "__main__":
    main()
