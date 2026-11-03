#!/usr/bin/env python3
"""Check that own keyboard events do not trigger takeover, on the fixture only.

Run with PYTHONPATH=sdk/python/src. Waits at most 30 seconds for an idle desktop
before opening one session; never retries a mutation or logs field contents.
"""
import argparse
import ctypes
import time

from nexus_cua import (
    AccessibilityMode, ActionKind, CapabilityManifest, Client, Config,
    FocusElement, FocusWindow, PermissionMode, PressKeys, SensitiveText, TypeText,
    select_application, select_window,
)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--endpoint", required=True)
    parser.add_argument("--token-file", required=True)
    args = parser.parse_args()
    cg = ctypes.CDLL("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics")
    idle = cg.CGEventSourceSecondsSinceLastEventType
    idle.argtypes = (ctypes.c_int32, ctypes.c_uint32)
    idle.restype = ctypes.c_double
    deadline = time.monotonic() + 30
    while idle(1, 0xffffffff) < 3:
        if time.monotonic() > deadline:
            raise RuntimeError("desktop remained active; probe not started")
        time.sleep(0.1)
    client = Client(Config(args.endpoint, args.token_file))
    app = select_application(client.discover_applications().applications,
                             "dev.nexus.cua.native-fixture")
    session = client.open_session(CapabilityManifest(
        mode=PermissionMode.BOUNDED, application_refs=(app.discovery_ref,),
        allowed_actions=(ActionKind.FOCUS_WINDOW, ActionKind.FOCUS_ELEMENT,
                         ActionKind.TYPE_TEXT, ActionKind.PRESS_KEYS),
        allow_foreground_input=True, ttl_seconds=60,
    ))
    try:
        window = select_window(client.list_windows(session.session_id), "Generation")

        def observe():
            return client.observe_window(session.session_id, window.window_ref,
                                         include_screenshot=False, accessibility=AccessibilityMode.FULL)

        def perform(observation, action):
            return client.perform_action(session.session_id, window.window_ref,
                                         observation.observation_id, action)

        perform(observe(), FocusWindow())
        observed = observe()
        field = next(element for element in observed.elements if element.name == "Fixture Text")
        perform(observed, FocusElement(field.element_ref))
        perform(observe(), PressKeys(("mod", "a")))
        perform(observe(), TypeText(SensitiveText("takeover-probe")))
        for _ in range(10):
            observed = observe()
            field = next(element for element in observed.elements if element.name == "Fixture Text")
            if field.value == "takeover-probe":
                print("own_keyboard_events_no_false_takeover passed")
                break
            time.sleep(0.05)
        else:
            raise RuntimeError("fixture text does not match the expected replacement")
    finally:
        client.close_session(session.session_id)


if __name__ == "__main__":
    main()
