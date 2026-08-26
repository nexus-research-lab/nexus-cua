from __future__ import annotations

from dataclasses import dataclass, field
import json
import os
from pathlib import Path
import secrets
import stat
import threading
import time
from typing import Any, Callable, Iterable, TypeVar

from . import _transport
from .types import (
    PROTOCOL_VERSION,
    AccessibilityMode,
    Action,
    ActionOutput,
    ApplicationSummary,
    CapabilityManifest,
    CUAError,
    DiscoverApplicationsOutput,
    DiscoveredApplication,
    DriverCapabilities,
    MutationStatus,
    OpenSessionOutput,
    PermissionStatus,
    StatePredicate,
    VerificationOutput,
    WindowObservation,
    WindowSummary,
    closed_object,
    action_wire,
    predicate_wire,
    string_value,
)


DEFAULT_MAX_FRAME_BYTES = 1024 * 1024
DEFAULT_RECONCILIATION_HORIZON = 600.0
MINIMUM_TOKEN_BYTES = 32
MAXIMUM_TOKEN_BYTES = 4096


@dataclass(frozen=True)
class Config:
    endpoint: str
    token_file: str | os.PathLike[str]
    max_frame_bytes: int = DEFAULT_MAX_FRAME_BYTES
    reconciliation_horizon: float = DEFAULT_RECONCILIATION_HORIZON


def select_application(
    applications: Iterable[DiscoveredApplication], selector: str
) -> DiscoveredApplication:
    """Select one discovered app without silently resolving ambiguity."""
    if not selector.strip():
        raise ValueError("application selector is required")
    candidates = tuple(applications)
    exact = tuple(
        application
        for application in candidates
        if application.name == selector or application.application_id == selector
    )
    if len(exact) == 1:
        return exact[0]
    if len(exact) > 1:
        raise ValueError(
            f"application selector {selector!r} has {len(exact)} exact matches"
        )
    partial = tuple(
        application
        for application in candidates
        if selector in application.name or selector in application.application_id
    )
    if len(partial) == 1:
        return partial[0]
    if not partial:
        raise LookupError(f"application selector {selector!r} did not match discovery")
    raise ValueError(
        f"application selector {selector!r} is ambiguous across {len(partial)} "
        "matches; use an exact name or stable application ID"
    )


def select_window(
    windows: Iterable[WindowSummary], selector: str
) -> WindowSummary:
    """Select one top-level window without relying on platform ordering."""
    if not selector.strip():
        raise ValueError("window selector is required")
    candidates = tuple(windows)
    exact = tuple(window for window in candidates if window.title == selector)
    if len(exact) == 1:
        return exact[0]
    if len(exact) > 1:
        raise ValueError(
            f"window selector {selector!r} has {len(exact)} exact matches"
        )
    partial = tuple(window for window in candidates if selector in window.title)
    if len(partial) == 1:
        return partial[0]
    if not partial:
        raise LookupError(
            f"window selector {selector!r} did not match the selected application"
        )
    raise ValueError(
        f"window selector {selector!r} is ambiguous across {len(partial)} "
        "matches; use an exact title"
    )


@dataclass
class ActionRequest:
    request_id: str
    _command: dict[str, Any] = field(repr=False)
    _created_at: float = field(repr=False)
    _last_timeout: float = field(default=0.0, repr=False)
    _completed: bool = field(default=False, repr=False)
    _invalidated: bool = field(default=False, repr=False)
    _lock: threading.Lock = field(default_factory=threading.Lock, repr=False)


class MutationIndeterminateError(Exception):
    def __init__(self, request: ActionRequest, cause: BaseException) -> None:
        super().__init__(
            f"nexus-cua: mutation {request.request_id} is indeterminate: {cause}"
        )
        self.request = request
        self.cause = cause


T = TypeVar("T")


class Client:
    def __init__(self, config: Config) -> None:
        if not config.endpoint:
            raise ValueError("endpoint is required")
        if config.max_frame_bytes <= 0 or config.max_frame_bytes > 2**32 - 1:
            raise ValueError("max_frame_bytes must fit a non-zero u32")
        if config.reconciliation_horizon < DEFAULT_RECONCILIATION_HORIZON:
            raise ValueError("reconciliation horizon must be at least ten minutes")
        self._endpoint = config.endpoint
        self._token = _read_private_token(Path(config.token_file))
        self._max_frame_bytes = config.max_frame_bytes
        self._reconciliation_horizon = config.reconciliation_horizon
        self._clock: Callable[[], float] = time.monotonic
        self._request_id: Callable[[], str] = lambda: f"request_{secrets.token_hex(16)}"
        self._round_tripper = _transport.round_trip

    def get_capabilities(self, timeout: float = 30.0) -> DriverCapabilities:
        return self._request_result(
            {"operation": "get_capabilities"},
            timeout,
            "capabilities",
            DriverCapabilities.from_wire,
        )

    def get_permission_status(self, timeout: float = 30.0) -> PermissionStatus:
        return self._request_result(
            {"operation": "get_permission_status"},
            timeout,
            "permission_status",
            PermissionStatus.from_wire,
        )

    def discover_applications(
        self, timeout: float = 30.0
    ) -> DiscoverApplicationsOutput:
        return self._request_result(
            {"operation": "discover_applications"},
            timeout,
            "applications_discovered",
            DiscoverApplicationsOutput.from_wire,
        )

    def open_session(
        self, manifest: CapabilityManifest, timeout: float = 30.0
    ) -> OpenSessionOutput:
        command = {
            "operation": "open_session",
            "input": {"manifest": manifest.to_wire()},
        }
        return self._request_result(
            command, timeout, "session_opened", OpenSessionOutput.from_wire
        )

    def close_session(self, session_id: str, timeout: float = 30.0) -> None:
        response = self._request(
            {"operation": "close_session", "input": {"session_id": session_id}}, timeout
        )
        self._decode_acknowledged(response)

    def list_apps(
        self, session_id: str, timeout: float = 30.0
    ) -> tuple[ApplicationSummary, ...]:
        return self._request_result(
            {"operation": "list_apps", "input": {"session_id": session_id}},
            timeout,
            "apps",
            lambda value: tuple(
                ApplicationSummary.from_wire(item) for item in _list(value)
            ),
        )

    def list_windows(
        self, session_id: str, app_ref: str | None = None, timeout: float = 30.0
    ) -> tuple[WindowSummary, ...]:
        return self._request_result(
            {
                "operation": "list_windows",
                "input": {"session_id": session_id, "app_ref": app_ref},
            },
            timeout,
            "windows",
            lambda value: tuple(WindowSummary.from_wire(item) for item in _list(value)),
        )

    def observe_window(
        self,
        session_id: str,
        window_ref: str,
        *,
        include_screenshot: bool = True,
        accessibility: AccessibilityMode = AccessibilityMode.INTERACTIVE,
        timeout: float = 30.0,
    ) -> WindowObservation:
        command = {
            "operation": "observe_window",
            "input": {
                "session_id": session_id,
                "window_ref": window_ref,
                "include_screenshot": include_screenshot,
                "accessibility": accessibility.value,
            },
        }
        return self._request_result(
            command, timeout, "window_observed", WindowObservation.from_wire
        )

    def verify_state(
        self,
        session_id: str,
        window_ref: str,
        predicate: StatePredicate,
        timeout: float = 30.0,
    ) -> VerificationOutput:
        command = {
            "operation": "verify_state",
            "input": {
                "session_id": session_id,
                "window_ref": window_ref,
                "predicate": predicate_wire(predicate),
            },
        }
        return self._request_result(
            command, timeout, "state_verified", VerificationOutput.from_wire
        )

    def perform_action(
        self,
        session_id: str,
        window_ref: str,
        observation_id: str,
        action: Action,
        timeout: float = 30.0,
    ) -> tuple[ActionOutput, ActionRequest]:
        command = {
            "operation": "perform_action",
            "input": {
                "session_id": session_id,
                "window_ref": window_ref,
                "observation_id": observation_id,
                "action": action_wire(action),
            },
        }
        request = ActionRequest(self._request_id(), command, self._clock())
        return self._wait_for_action(request, timeout), request

    def reconcile_action(self, request: ActionRequest, timeout: float) -> ActionOutput:
        if not isinstance(request, ActionRequest):
            raise TypeError("request must be an ActionRequest")
        return self._wait_for_action(request, timeout)

    def _wait_for_action(self, request: ActionRequest, timeout: float) -> ActionOutput:
        with request._lock:
            if request._completed:
                raise ValueError("mutation request is already complete")
            if (
                request._invalidated
                or self._clock() - request._created_at > self._reconciliation_horizon
            ):
                request._invalidated = True
                request._command.clear()
                raise MutationIndeterminateError(
                    request, RuntimeError("reconciliation horizon elapsed")
                )
            if request._last_timeout and timeout < request._last_timeout:
                raise ValueError(
                    "reconciliation may only extend the prior wait deadline"
                )
            _timeout_milliseconds(timeout)
            request._last_timeout = timeout
            try:
                response = self._request_with_id(
                    request.request_id, request._command, timeout
                )
                result = self._decode_result(
                    response, "action_performed", ActionOutput.from_wire
                )
            except CUAError as error:
                if error.mutation_status is MutationStatus.NOT_DISPATCHED:
                    request._completed = True
                    request._command.clear()
                    raise
                raise MutationIndeterminateError(request, error) from error
            except Exception as error:
                raise MutationIndeterminateError(request, error) from error
            request._completed = True
            request._command.clear()
            return result

    def _request_result(
        self,
        command: dict[str, Any],
        timeout: float,
        result_type: str,
        parser: Callable[[Any], T],
    ) -> T:
        return self._decode_result(self._request(command, timeout), result_type, parser)

    def _request(self, command: dict[str, Any], timeout: float) -> dict[str, Any]:
        return self._request_with_id(self._request_id(), command, timeout)

    def _request_with_id(
        self, request_id: str, command: dict[str, Any], timeout: float
    ) -> dict[str, Any]:
        timeout_ms = _timeout_milliseconds(timeout)
        envelope = {
            "protocol_version": PROTOCOL_VERSION,
            "request_id": request_id,
            "timeout_ms": timeout_ms,
            "authorization": self._token,
            "command": command,
        }
        payload = json.dumps(
            envelope, separators=(",", ":"), ensure_ascii=False, allow_nan=False
        ).encode("utf-8")
        response = self._round_tripper(
            self._endpoint, payload, self._max_frame_bytes, self._clock() + timeout
        )
        try:
            decoded = json.loads(response, parse_constant=_reject_json_constant)
        except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
            raise _transport.TransportError(f"invalid sidecar JSON: {error}") from error
        data = closed_object(decoded, {"protocol_version", "request_id", "outcome"})
        if string_value(data["protocol_version"]) != PROTOCOL_VERSION:
            raise _transport.TransportError("unsupported response protocol")
        if string_value(data["request_id"]) != request_id:
            raise _transport.TransportError("response request_id mismatch")
        return data

    @staticmethod
    def _decode_result(
        response: dict[str, Any], expected: str, parser: Callable[[Any], T]
    ) -> T:
        outcome = response["outcome"]
        if not isinstance(outcome, dict):
            raise ValueError("outcome must be an object")
        status = outcome.get("status")
        if status == "error":
            data = closed_object(outcome, {"status", "error"})
            raise CUAError.from_wire(data["error"])
        if status != "success":
            raise ValueError(f"unknown outcome status {status!r}")
        data = closed_object(outcome, {"status", "result"})
        result = closed_object(data["result"], {"result_type", "data"})
        result_type = string_value(result["result_type"])
        if result_type != expected:
            raise ValueError(f"expected result {expected!r}, received {result_type!r}")
        return parser(result["data"])

    @staticmethod
    def _decode_acknowledged(response: dict[str, Any]) -> None:
        outcome = response["outcome"]
        if not isinstance(outcome, dict):
            raise ValueError("outcome must be an object")
        status = outcome.get("status")
        if status == "error":
            data = closed_object(outcome, {"status", "error"})
            raise CUAError.from_wire(data["error"])
        if status != "success":
            raise ValueError(f"unknown outcome status {status!r}")
        data = closed_object(outcome, {"status", "result"})
        result = closed_object(data["result"], {"result_type"})
        if string_value(result["result_type"]) != "acknowledged":
            raise ValueError("expected acknowledged result")


def _timeout_milliseconds(timeout: float) -> int:
    if timeout <= 0:
        raise ValueError("timeout must be positive")
    milliseconds = int(timeout * 1000)
    if milliseconds <= 0 or milliseconds > 2**32 - 1:
        raise ValueError("timeout is outside the wire bound")
    return milliseconds


def _read_private_token(path: Path) -> str:
    metadata = path.stat()
    if not stat.S_ISREG(metadata.st_mode):
        raise ValueError("token path must be a regular file")
    if os.name != "nt" and stat.S_IMODE(metadata.st_mode) & 0o077:
        raise ValueError("token file must not be accessible by group or other users")
    value = path.read_text(encoding="utf-8").strip()
    if not (MINIMUM_TOKEN_BYTES <= len(value) <= MAXIMUM_TOKEN_BYTES) or any(
        character.isspace() for character in value
    ):
        raise ValueError("token file must contain 32 to 4096 non-whitespace bytes")
    return value


def _list(value: Any) -> list[Any]:
    if not isinstance(value, list):
        raise ValueError("expected a list result")
    return value


def _reject_json_constant(value: str) -> Any:
    raise ValueError(f"non-standard JSON constant {value!r}")
