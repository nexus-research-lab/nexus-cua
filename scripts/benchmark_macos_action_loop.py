#!/usr/bin/env python3
"""Bounded local action/observe timings on the owned AppKit fixture only.

Run with PYTHONPATH=sdk/python/src. Requires the native fixture to be visible.
Prints only timings and boolean assertions, never screenshots or AX contents.
This is a single-host diagnostic, not a release acceptance or general benchmark.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import secrets
import statistics
import subprocess
import tempfile
import time

from nexus_cua import (
    AccessibilityMode, ActionKind, CapabilityManifest, Client, Config,
    InvokeElement, ObserveAfterOptions, PermissionMode, select_application, select_window,
)


def counter(observation):
    for element in observation.elements:
        match = re.search(r"Fixture State:.*counter=(\d+);", element.name or "")
        if match:
            return int(match.group(1))
    raise RuntimeError("fixture counter evidence missing")


def measure(client, count, combined):
    app = select_application(client.discover_applications().applications,
                             "dev.nexus.cua.native-fixture")
    session = client.open_session(CapabilityManifest(
        mode=PermissionMode.BOUNDED, application_refs=(app.discovery_ref,),
        allowed_actions=(ActionKind.INVOKE_ELEMENT,),
        allow_foreground_input=False, ttl_seconds=120,
    ))
    try:
        window = select_window(client.list_windows(session.session_id), "Generation")

        def observe():
            return client.observe_window(session.session_id, window.window_ref,
                                         accessibility=AccessibilityMode.FULL)

        observed = observe()
        for _ in range(2):
            observed = observe()
        action_ms, loop_ms = [], []
        replay_checked = False
        for _ in range(count):
            before = counter(observed)
            target = next(e for e in observed.elements if e.name == "Increment Counter")
            started = time.perf_counter()
            replay_command = {"operation": "perform_action", "input": {
                "session_id": session.session_id, "window_ref": window.window_ref,
                "observation_id": observed.observation_id,
                "action": {"kind": "invoke_element", "element_ref": target.element_ref},
            }}
            if combined:
                replay_command["input"]["observe_after"] = ObserveAfterOptions(accessibility=AccessibilityMode.FULL).to_wire()
            output, request = client.perform_action(
                session.session_id, window.window_ref, observed.observation_id,
                InvokeElement(target.element_ref),
                observe_after=ObserveAfterOptions(accessibility=AccessibilityMode.FULL) if combined else None,
            )
            action_ms.append((time.perf_counter() - started) * 1000)
            if output.observation_error is not None:
                raise RuntimeError("post-action observation failed")
            observed = output.observation if combined else observe()
            if observed is None or counter(observed) != before + 1:
                raise RuntimeError("counter did not change exactly once")
            loop_ms.append((time.perf_counter() - started) * 1000)
            if combined and not replay_checked:
                # Exercise dispatcher replay with the exact identity/options,
                # including its original returned observation, without mutation.
                replay = client._request_with_id(request.request_id, replay_command, 30)
                data = replay["outcome"]["result"]["data"]
                if data["observation"]["observation_id"] != observed.observation_id:
                    raise RuntimeError("replay did not retain the original observation")
                checked = observe()
                if counter(checked) != before + 1:
                    raise RuntimeError("replay duplicated the mutation")
                observed = checked
                replay_checked = True
        def stats(values):
            ordered = sorted(values)
            return {"median_ms": round(statistics.median(values), 2),
                    "p95_ms": round(ordered[min(len(ordered)-1, int(len(ordered)*0.95))], 2)}
        return {"samples": count, "combined": combined, "action": stats(action_ms),
                "action_and_observation": stats(loop_ms), "counter_verified": True,
                "same_id_replay_verified": replay_checked}
    finally:
        client.close_session(session.session_id)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--samples", type=int, default=12)
    parser.add_argument("--combined", action="store_true")
    args = parser.parse_args()
    if not 3 <= args.samples <= 50:
        parser.error("samples must be 3..50")
    binary = args.binary.resolve()
    with tempfile.TemporaryDirectory(prefix="ncuaperf-", dir="/tmp") as directory:
        root = Path(directory)
        token = root / "token"
        token.write_text(secrets.token_hex(32))
        token.chmod(0o600)
        endpoint = root / "runtime.sock"
        with (root / "runtime.log").open("w") as log:
            process = subprocess.Popen([str(binary), "serve", "--endpoint", str(endpoint),
                "--token-file", str(token), "--artifact-root", str(root / "artifacts"),
                "--log-level", "warn"], stdout=log, stderr=log)
            try:
                for _ in range(100):
                    if endpoint.exists():
                        break
                    if process.poll() is not None:
                        raise RuntimeError("runtime exited before readiness")
                    time.sleep(0.05)
                client = Client(Config(str(endpoint), token))
                capabilities = client.get_capabilities()
                if args.combined and not capabilities.observe_after:
                    raise RuntimeError("runtime does not support observe_after")
                result = measure(client, args.samples, args.combined)
                result["runtime_version"] = capabilities.runtime_version
                result["binary_sha256"] = hashlib.sha256(binary.read_bytes()).hexdigest()
                print(json.dumps(result))
            finally:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)


if __name__ == "__main__":
    main()
