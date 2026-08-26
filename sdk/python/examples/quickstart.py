from __future__ import annotations

import argparse

from nexus_cua import (
    AccessibilityMode,
    ActionKind,
    CapabilityManifest,
    Client,
    Config,
    InvokeElement,
    PermissionMode,
    select_application,
    select_window,
)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--endpoint", required=True)
    parser.add_argument("--token-file", required=True)
    parser.add_argument(
        "--application", required=True, help="name or stable ID substring"
    )
    args = parser.parse_args()

    client = Client(Config(args.endpoint, args.token_file))
    discovery = client.discover_applications()
    selected = select_application(discovery.applications, args.application)
    session = client.open_session(
        CapabilityManifest(
            mode=PermissionMode.BOUNDED,
            application_refs=(selected.discovery_ref,),
            allowed_actions=(ActionKind.INVOKE_ELEMENT,),
            ttl_seconds=60,
        )
    )
    try:
        window = select_window(
            client.list_windows(session.session_id), args.application
        )
        observation = client.observe_window(
            session.session_id,
            window.window_ref,
            include_screenshot=True,
            accessibility=AccessibilityMode.INTERACTIVE,
        )
        increment = next(
            element
            for element in observation.elements
            if element.name == "Increment Counter" and "invoke" in element.actions
        )
        action, _ = client.perform_action(
            session.session_id,
            window.window_ref,
            observation.observation_id,
            InvokeElement(increment.element_ref),
        )
        updated = client.observe_window(
            session.session_id,
            window.window_ref,
            include_screenshot=False,
            accessibility=AccessibilityMode.INTERACTIVE,
        )
        print(
            f"window={window.title!r} elements={len(updated.elements)} "
            f"screenshot={observation.screenshot is not None} "
            f"mutation={action.delivery_mode.value}/{action.dispatched}"
        )
    finally:
        client.close_session(session.session_id)


if __name__ == "__main__":
    main()
