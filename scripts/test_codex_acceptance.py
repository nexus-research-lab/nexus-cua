import importlib.util
import json
import tempfile
import unittest
from importlib.machinery import SourceFileLoader
from pathlib import Path


ROOT = Path(__file__).resolve().parent
LOADER = SourceFileLoader("run_codex_acceptance", str(ROOT / "run_codex_acceptance"))
SPEC = importlib.util.spec_from_loader("run_codex_acceptance", LOADER)
assert SPEC and SPEC.loader
HARNESS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HARNESS)


def accepted_attempt(index: int = 1) -> dict:
    sequence = list(HARNESS.REQUIRED_EVENT_SEQUENCE)
    return {
        "attempt_index": index,
        "status": "accepted",
        "actual_tool_sequence": sequence,
        "tool_events": [{"server": "nexuscua", "tool": tool} for tool in sequence],
        "request_ids": {
            "launch": f"a{index}:launch",
            "click": f"a{index}:click",
            "toggle": f"a{index}:toggle",
            "terminate": f"a{index}:terminate",
        },
        "state_assertions": {
            "initial_counter_zero": True,
            "initial_checked_false": True,
            "counter_one_after_click": True,
            "checked_true_after_toggle": True,
            "final_counter_one": True,
            "final_checked_true": True,
            "final_applications_empty": True,
            "final_windows_empty": True,
        },
        "policy_violations": [],
        "cleanup": {
            "fixture_absent": True,
            "registration_removed": True,
            "artifacts_removed": True,
        },
        "timings_ms": {"total": 1},
    }


class CodexAcceptanceTests(unittest.TestCase):
    def test_event_extraction_requires_explicit_machine_event(self) -> None:
        self.assertIsNone(HARNESS.extract_acceptance(["the fixture passed"]
        ))
        result = {"status": "accepted"}
        self.assertEqual(
            HARNESS.extract_acceptance([json.dumps({"nexus_cua_acceptance": result})]),
            result,
        )

    def test_attempt_contract_rejects_shell_or_prefixed_tools(self) -> None:
        attempt = accepted_attempt()
        attempt["actual_tool_sequence"].insert(1, "shell")
        attempt["tool_events"].insert(1, {"server": "nexuscua", "tool": "shell"})
        self.assertTrue(HARNESS.validate_attempt(attempt, 1))

    def test_attempt_contract_accepts_exact_flow(self) -> None:
        self.assertEqual(HARNESS.validate_attempt(accepted_attempt(), 1), [])

    def test_runner_manifest_is_pinned_to_macos_arm64(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "runner.json"
            path.write_text(
                json.dumps(
                    {
                        "runner_id": "mac-1",
                        "status": "active",
                        "platform": "macos",
                        "architecture": "aarch64",
                        "cpu": {},
                        "memory_bytes": 1,
                        "display_topology": {},
                        "os_build": {},
                        "toolchain": {},
                        "signing_identity": {"identifier": "io.nexus.cua.hardware-sidecar"},
                    }
                ),
                encoding="utf-8",
            )
            self.assertEqual(HARNESS.validate_runner(path)["runner_id"], "mac-1")


if __name__ == "__main__":
    unittest.main()
