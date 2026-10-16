from __future__ import annotations

import json
import os
from pathlib import Path
import time
import tempfile
import unittest
import struct
from types import SimpleNamespace

import nexus_cua


REPOSITORY = Path(__file__).resolve().parents[3]
FIXTURES = REPOSITORY / "fixtures" / "compatibility" / "nexus.cua.v1"


class ScriptedTransport:
    def __init__(self, results: dict[str, object]) -> None:
        self.results = results
        self.requests: list[dict[str, object]] = []
        self.fail_once = False

    def __call__(
        self, endpoint: str, payload: bytes, limit: int, deadline: float
    ) -> bytes:
        del endpoint, limit, deadline
        request = json.loads(payload)
        self.requests.append(request)
        if self.fail_once:
            self.fail_once = False
            raise TimeoutError("fixture timeout")
        result = self.results[request["command"]["operation"]]
        return json.dumps(
            {
                "protocol_version": nexus_cua.PROTOCOL_VERSION,
                "request_id": request["request_id"],
                "outcome": {"status": "success", "result": result},
            }
        ).encode()


class ClientTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.token_file = Path(self.temporary.name) / "token"
        self.token_file.write_text("x" * 64, encoding="utf-8")
        self.token_file.chmod(0o600)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def client(self, transport: ScriptedTransport) -> nexus_cua.Client:
        client = nexus_cua.Client(nexus_cua.Config("fixture-endpoint", self.token_file))
        client._round_tripper = transport
        return client

    def test_compatibility_fixtures_cover_public_client(self) -> None:
        requests = json.loads((FIXTURES / "requests.json").read_text())
        results = json.loads((FIXTURES / "results.json").read_text())
        result_by_operation = {
            request["command"]["operation"]: results[index]
            for index, request in enumerate(requests)
        }
        transport = ScriptedTransport(result_by_operation)
        client = self.client(transport)
        client._token = "fixture-transport-token"
        request_ids = iter(request["request_id"] for request in requests)
        client._request_id = lambda: next(request_ids)

        client.get_capabilities()
        client.get_permission_status()
        client.discover_applications()
        client.discover_launchable_applications()
        client.launch_application(
            "launch_fixture", allow_foreground_activation=True
        )
        client.open_session(
            nexus_cua.CapabilityManifest(
                nexus_cua.PermissionMode.BOUNDED,
                ("discovery_fixture",),
                (
                    nexus_cua.ActionKind.FOCUS_WINDOW,
                    nexus_cua.ActionKind.INVOKE_ELEMENT,
                    nexus_cua.ActionKind.SET_RANGE_VALUE,
                    nexus_cua.ActionKind.CLICK_POINT,
                ),
                True,
                300,
                True,
                True,
                True,
                True,
            )
        )
        client.close_session("session_fixture")
        client.list_apps("session_fixture")
        client.list_windows("session_fixture", "app_fixture")
        client.list_displays("session_fixture")
        client.observe_display("session_fixture", "display_fixture")
        client.observe_window("session_fixture", "window_fixture")
        client.read_clipboard("session_fixture")
        client.write_clipboard(
            "session_fixture",
            "clipboard_fixture",
            nexus_cua.SensitiveText("replacement fixture"),
        )
        client.perform_action(
            "session_fixture",
            "window_fixture",
            "observation_fixture",
            nexus_cua.FocusWindow(),
        )
        client.verify_state(
            "session_fixture",
            "window_fixture",
            nexus_cua.ElementStateMatches(
                state=nexus_cua.ElementState.CHECKED,
                value=True,
                role="checkbox",
                name="Remember me",
            ),
        )
        client.reconcile_mutation("fixture-action", wait=0)

        self.assertEqual(transport.requests, requests)

    def test_every_stable_error_fixture_decodes(self) -> None:
        errors = json.loads((FIXTURES / "errors.json").read_text())
        self.assertEqual(
            {nexus_cua.CUAError.from_wire(value).code for value in errors},
            set(nexus_cua.ErrorCode),
        )

    def test_closed_values_and_unknown_fields_fail(self) -> None:
        with self.assertRaises(ValueError):
            nexus_cua.Platform("linux")
        with self.assertRaises(ValueError):
            nexus_cua.DriverCapabilities.from_wire(
                {
                    "protocol_version": "nexus.cua.v1",
                    "runtime_version": "x",
                    "platform": "macos",
                    "capture_modes": [],
                    "accessibility_tree": False,
                    "input_routes": [],
                    "actions": [],
                    "extra": True,
                }
            )
        with self.assertRaises(ValueError):
            nexus_cua.ActionOutput.from_wire(
                {
                    "delivery_mode": "system",
                    "dispatched": True,
                    "effect_status": "confirmed",
                    "evidence_kind": "dispatch_acceptance",
                    "foreground_restoration": "not_requested",
                    "observation_invalidated": True,
                }
            )
        with self.assertRaises(ValueError):
            nexus_cua.ForegroundRestorationStatus("future_status")
        with self.assertRaises(ValueError):
            nexus_cua.CapabilityManifest(
                nexus_cua.PermissionMode.BOUNDED,
                ("discovery_fixture",),
                restore_foreground_after_input=True,
            ).to_wire()

    def test_sensitive_text_is_redacted(self) -> None:
        secret = nexus_cua.SensitiveText("do-not-print")
        self.assertEqual(str(secret), "[REDACTED]")
        self.assertEqual(repr(secret), "SensitiveText([REDACTED])")
        self.assertEqual(secret.expose(), "do-not-print")

    def test_application_selection_prefers_exact_and_rejects_ambiguity(self) -> None:
        applications = (
            SimpleNamespace(
                name="AutoFill (Fixture)", application_id="com.example.helper"
            ),
            SimpleNamespace(name="Fixture", application_id="dev.example.fixture"),
        )
        selected = nexus_cua.select_application(applications, "Fixture")
        self.assertEqual(selected.application_id, "dev.example.fixture")
        with self.assertRaises(ValueError):
            nexus_cua.select_application(applications, "example")
        with self.assertRaises(LookupError):
            nexus_cua.select_application(applications, "Missing")

    def test_window_selection_does_not_depend_on_platform_ordering(self) -> None:
        windows = (
            SimpleNamespace(title=""),
            SimpleNamespace(title="Fixture · Generation 1"),
            SimpleNamespace(title="Fixture Help"),
        )
        selected = nexus_cua.select_window(windows, "Generation 1")
        self.assertEqual(selected.title, "Fixture · Generation 1")
        with self.assertRaises(ValueError):
            nexus_cua.select_window(windows, "Fixture")

    def test_client_representation_and_frame_boundaries(self) -> None:
        client = self.client(ScriptedTransport({}))
        self.assertNotIn("x" * 64, repr(client))
        from nexus_cua import _transport

        fixture = json.loads((FIXTURES / "frame-boundaries.json").read_text())
        maximum = fixture["configured_max_bytes"]
        self.assertEqual(
            _transport._response_length(struct.pack(">I", maximum), maximum), maximum
        )
        with self.assertRaises(_transport.TransportError):
            _transport._response_length(struct.pack(">I", maximum + 1), maximum)

    @unittest.skipUnless(os.name == "nt", "Windows named-pipe declarations")
    def test_windows_named_pipe_module_imports(self) -> None:
        from nexus_cua import _windows_pipe

        self.assertTrue(callable(_windows_pipe.pipe_round_trip))

    @unittest.skipUnless(os.name == "nt", "Windows named-pipe transport")
    def test_windows_named_pipe_failures_are_transport_errors(self) -> None:
        from nexus_cua import _transport, _windows_pipe

        with self.assertRaises(_transport.TransportError):
            _windows_pipe.pipe_round_trip(
                r"\\.\pipe\nexus-cua-sdk-missing-pipe",
                b"{}",
                1024,
                time.monotonic() + 0.05,
            )

    def test_mutation_reconciliation_reuses_identity_and_extends_wait(self) -> None:
        transport = ScriptedTransport(
            {
                "perform_action": {
                    "result_type": "action_performed",
                    "data": {
                        "delivery_mode": "semantic",
                        "dispatched": True,
                        "effect_status": "unverified",
                        "evidence_kind": "dispatch_acceptance",
                        "foreground_restoration": "not_requested",
                        "observation_invalidated": True,
                    },
                }
            }
        )
        transport.fail_once = True
        client = self.client(transport)
        client._request_id = lambda: "request_reconcile"
        with self.assertRaises(nexus_cua.MutationIndeterminateError) as captured:
            client.perform_action(
                "session", "window", "observation", nexus_cua.FocusWindow(), timeout=1.0
            )
        request = captured.exception.request
        with self.assertRaises(ValueError):
            client.reconcile_action(request, 0.5)
        result = client.reconcile_action(request, 2.0)
        self.assertTrue(result.dispatched)
        self.assertEqual(len(transport.requests), 2)
        self.assertEqual(
            transport.requests[0]["request_id"], transport.requests[1]["request_id"]
        )
        self.assertEqual(
            transport.requests[0]["command"], transport.requests[1]["command"]
        )
        self.assertLess(
            transport.requests[0]["timeout_ms"], transport.requests[1]["timeout_ms"]
        )

    def test_launch_reconciliation_reuses_identity_and_extends_wait(self) -> None:
        transport = ScriptedTransport(
            {
                "launch_application": {
                    "result_type": "application_launched",
                    "data": {
                        "application": {
                            "discovery_ref": "discovery_launched",
                            "name": "Fixture",
                            "application_id": "dev.nexus.fixture",
                            "foreground": True,
                            "provenance": {
                                "platform": "macos",
                                "bundle_id": "dev.nexus.fixture",
                                "executable_path": "/Applications/Fixture.app/Contents/MacOS/Fixture",
                                "signing_team_id": None,
                                "designated_requirement": None,
                            },
                            "expires_at": "2026-08-25T12:00:30Z",
                        },
                        "delivery_mode": "foreground",
                        "dispatched": True,
                    },
                }
            }
        )
        transport.fail_once = True
        client = self.client(transport)
        client._request_id = lambda: "request_launch_reconcile"
        with self.assertRaises(nexus_cua.LaunchIndeterminateError) as captured:
            client.launch_application(
                "launch_fixture", allow_foreground_activation=True, timeout=1.0
            )
        request = captured.exception.request
        with self.assertRaises(ValueError):
            client.reconcile_launch(request, 0.5)
        result = client.reconcile_launch(request, 2.0)
        self.assertTrue(result.dispatched)
        self.assertEqual(result.application.application_id, "dev.nexus.fixture")
        self.assertEqual(len(transport.requests), 2)
        self.assertEqual(
            transport.requests[0]["request_id"], transport.requests[1]["request_id"]
        )
        self.assertEqual(
            transport.requests[0]["command"], transport.requests[1]["command"]
        )
        self.assertLess(
            transport.requests[0]["timeout_ms"], transport.requests[1]["timeout_ms"]
        )

    def test_clipboard_reconciliation_reuses_identity_and_hides_text(self) -> None:
        transport = ScriptedTransport(
            {
                "write_clipboard": {
                    "result_type": "clipboard_written",
                    "data": {
                        "clipboard_ref": "clipboard_after",
                        "delivery_mode": "system",
                        "dispatched": True,
                        "effect_status": "confirmed",
                        "evidence_kind": "native_readback",
                    },
                }
            }
        )
        transport.fail_once = True
        client = self.client(transport)
        client._request_id = lambda: "request_clipboard_reconcile"
        with self.assertRaises(nexus_cua.ClipboardIndeterminateError) as captured:
            client.write_clipboard(
                "session",
                "clipboard_before",
                nexus_cua.SensitiveText("do-not-print"),
                timeout=1.0,
            )
        request = captured.exception.request
        self.assertNotIn("do-not-print", repr(request))
        with self.assertRaises(ValueError):
            client.reconcile_clipboard(request, 0.5)
        result = client.reconcile_clipboard(request, 2.0)
        self.assertTrue(result.dispatched)
        self.assertEqual(len(transport.requests), 2)
        self.assertEqual(
            transport.requests[0]["request_id"], transport.requests[1]["request_id"]
        )
        self.assertEqual(
            transport.requests[0]["command"], transport.requests[1]["command"]
        )

    @unittest.skipUnless(
        os.environ.get("NEXUS_CUA_LIVE_ENDPOINT")
        and os.environ.get("NEXUS_CUA_LIVE_TOKEN_FILE"),
        "live sidecar environment is not configured",
    )
    def test_live_sidecar(self) -> None:
        client = nexus_cua.Client(
            nexus_cua.Config(
                os.environ["NEXUS_CUA_LIVE_ENDPOINT"],
                os.environ["NEXUS_CUA_LIVE_TOKEN_FILE"],
            )
        )
        capabilities = client.get_capabilities(timeout=10)
        self.assertEqual(capabilities.protocol_version, nexus_cua.PROTOCOL_VERSION)
        client.get_permission_status(timeout=10)
        discovery = client.discover_applications(timeout=10)
        application_match = os.environ.get("NEXUS_CUA_LIVE_APPLICATION_MATCH")
        if not application_match:
            return
        selected = nexus_cua.select_application(
            discovery.applications, application_match
        )
        mutate = os.environ.get("NEXUS_CUA_LIVE_MUTATION") == "1"
        manifest = nexus_cua.CapabilityManifest(
            nexus_cua.PermissionMode.BOUNDED
            if mutate
            else nexus_cua.PermissionMode.READ_ONLY,
            (selected.discovery_ref,),
            (nexus_cua.ActionKind.INVOKE_ELEMENT,) if mutate else (),
            False,
            60,
        )
        session = client.open_session(manifest, timeout=10)
        try:
            window_match = os.environ.get(
                "NEXUS_CUA_LIVE_WINDOW_MATCH",
                "Nexus CUA Native Fixture · Generation ",
            )
            window = nexus_cua.select_window(
                client.list_windows(session.session_id, timeout=10), window_match
            )
            observation = client.observe_window(
                session.session_id,
                window.window_ref,
                include_screenshot=False,
                timeout=20,
            )
            if mutate:
                increment = next(
                    element
                    for element in observation.elements
                    if element.name == "Increment Counter"
                )
                client.perform_action(
                    session.session_id,
                    window.window_ref,
                    observation.observation_id,
                    nexus_cua.InvokeElement(increment.element_ref),
                    timeout=20,
                )
                client.observe_window(
                    session.session_id,
                    window.window_ref,
                    include_screenshot=False,
                    timeout=20,
                )
        finally:
            client.close_session(session.session_id, timeout=10)


if __name__ == "__main__":
    unittest.main()
