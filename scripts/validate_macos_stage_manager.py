#!/usr/bin/env python3
"""Bounded live Stage Manager recovery probe for the owned macOS fixture only.

Run with PYTHONPATH=sdk/python/src. Does not change desktop preferences or replay
mutations. Prints content-free results; session close removes screenshots.
"""
from __future__ import annotations

import argparse
import time

from nexus_cua import (
    AccessibilityMode, ActionKind, CapabilityManifest, Client, Config, CUAError,
    ErrorCode, FocusWindow, InvokeElement, MutationStatus, PermissionMode,
    SetWindowMinimized, select_application, select_window,
)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--endpoint", required=True)
    parser.add_argument("--token-file", required=True)
    args = parser.parse_args()
    client = Client(Config(args.endpoint, args.token_file))
    app = select_application(client.discover_applications().applications,
                             "dev.nexus.cua.native-fixture")
    session = client.open_session(CapabilityManifest(
        mode=PermissionMode.BOUNDED, application_refs=(app.discovery_ref,),
        allowed_actions=(ActionKind.FOCUS_WINDOW, ActionKind.INVOKE_ELEMENT,
                         ActionKind.SET_WINDOW_MINIMIZED),
        allow_foreground_input=True, ttl_seconds=120,
    ))
    try:
        window = select_window(client.list_windows(session.session_id), "Generation")
        initial = client.observe_window(session.session_id, window.window_ref,
                                        include_screenshot=False, accessibility=AccessibilityMode.DISABLED)
        client.perform_action(session.session_id, window.window_ref, initial.observation_id, FocusWindow())
        for attempt in range(3):
            full = client.observe_window(session.session_id, window.window_ref,
                                         include_screenshot=False, accessibility=AccessibilityMode.FULL)
            minimize = next(element for element in full.elements
                            if element.name == "Minimize Fixture Window" and "invoke" in element.actions)
            client.perform_action(session.session_id, window.window_ref, full.observation_id,
                                  InvokeElement(minimize.element_ref))
            staged = False
            previous_bounds = None
            stable_samples = 0
            for _ in range(30):
                current = next(value for value in client.list_windows(session.session_id)
                               if value.window_ref == window.window_ref)
                stable_samples = stable_samples + 1 if current.screen_bounds == previous_bounds else 0
                previous_bounds = current.screen_bounds
                if current.screen_bounds.width < full.window_screen_bounds.width / 2 and stable_samples >= 2:
                    staged = True
                    break
                time.sleep(0.05)
            if not staged:
                raise RuntimeError("Stage Manager thumbnail transition not observed; this is not the standard minimized-window test")
            guard = client.observe_window(session.session_id, window.window_ref,
                                          include_screenshot=False, accessibility=AccessibilityMode.DISABLED)
            try:
                client.perform_action(session.session_id, window.window_ref, guard.observation_id,
                                      SetWindowMinimized(False))
            except CUAError as error:
                if (error.code is not ErrorCode.TARGET_UNAVAILABLE
                        or error.mutation_status is not MutationStatus.NOT_DISPATCHED
                        or error.recovery_action != "focus_window"):
                    raise RuntimeError(f"preflight returned {error.code.value}/{error.mutation_status.value}") from None
            else:
                raise RuntimeError("transformed AX target unexpectedly accepted restore")
            # Failed admission consumes the old observation, but not the grant.
            guard = client.observe_window(session.session_id, window.window_ref,
                                          include_screenshot=False, accessibility=AccessibilityMode.DISABLED)
            client.perform_action(session.session_id, window.window_ref, guard.observation_id, FocusWindow())
            restored = client.observe_window(session.session_id, window.window_ref,
                                             include_screenshot=True, accessibility=AccessibilityMode.FULL)
            if not any(element.name == "Increment Counter" for element in restored.elements):
                raise RuntimeError("restored window has no fixture controls")
            if restored.window_screen_bounds.width < full.window_screen_bounds.width / 2:
                raise RuntimeError("restored observation still uses thumbnail geometry")
            print(f"stage_manager_recovery attempt={attempt + 1} passed restore_not_dispatched=true same_session=true", flush=True)
    finally:
        client.close_session(session.session_id)


if __name__ == "__main__":
    main()
