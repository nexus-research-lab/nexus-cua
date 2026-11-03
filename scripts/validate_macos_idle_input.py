#!/usr/bin/env python3
"""Verify bounded idle recovery against the owned native fixture, without logging content.

Run with PYTHONPATH=sdk/python/src against an explicitly started test runtime.
An outside mouse event must invalidate the old observation without revoking the
session. This foreign-process probe does not replace physical-input acceptance.
"""
from __future__ import annotations

import argparse
import subprocess
import sys
import time

from nexus_cua import (
    AccessibilityMode, ActionKind, CapabilityManifest, Client, Config, CUAError,
    ErrorCode, FocusWindow, MutationStatus, PermissionMode, select_application, select_window,
)
from validate_macos_takeover import FOREIGN_MOVE


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
        allowed_actions=(ActionKind.FOCUS_WINDOW,), allow_foreground_input=True,
        ttl_seconds=60,
    ))
    try:
        window = select_window(client.list_windows(session.session_id), "Generation")

        def observe():
            return client.observe_window(session.session_id, window.window_ref,
                                         include_screenshot=False, accessibility=AccessibilityMode.DISABLED)

        observation = observe()
        client.perform_action(session.session_id, window.window_ref, observation.observation_id, FocusWindow())
        observation = observe()
        subprocess.run([sys.executable, "-c", FOREIGN_MOVE], check=True, timeout=2)
        time.sleep(0.1)  # Allow the passive run-loop callback to observe the injected event.
        started = time.monotonic()
        try:
            client.perform_action(session.session_id, window.window_ref, observation.observation_id, FocusWindow())
        except CUAError as error:
            if error.code is not ErrorCode.STALE_OBSERVATION or error.mutation_status is not MutationStatus.NOT_DISPATCHED:
                raise RuntimeError(f"idle input returned {error.code.value}/{error.mutation_status.value}") from None
        else:
            raise RuntimeError("pre-input observation unexpectedly dispatched")
        elapsed = time.monotonic() - started
        if not 1.5 <= elapsed <= 5.5:
            raise RuntimeError(f"idle wait outside expected bounded range: {elapsed:.2f}s")
        observation = observe()
        output, _ = client.perform_action(session.session_id, window.window_ref, observation.observation_id, FocusWindow())
        if not output.dispatched:
            raise RuntimeError("fresh observation did not resume the same session")
        print(f"idle_input passed mode=foreign_synthetic same_session=true old_observation_rejected=true wait_ms={elapsed * 1000:.1f}")
    finally:
        client.close_session(session.session_id)


if __name__ == "__main__":
    main()
