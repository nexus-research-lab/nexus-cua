from __future__ import annotations

from dataclasses import dataclass
from enum import Enum
import math
from typing import Any, Mapping, NewType, TypeVar


PROTOCOL_VERSION = "nexus.cua.v1"

RequestID = NewType("RequestID", str)
SessionID = NewType("SessionID", str)
AppRef = NewType("AppRef", str)
DiscoveryRef = NewType("DiscoveryRef", str)
WindowRef = NewType("WindowRef", str)
ObservationID = NewType("ObservationID", str)
ElementRef = NewType("ElementRef", str)
ArtifactRef = NewType("ArtifactRef", str)


class ClosedString(str, Enum):
    def __str__(self) -> str:
        return self.value


class Platform(ClosedString):
    MACOS = "macos"
    WINDOWS = "windows"
    UNSUPPORTED = "unsupported"


class CaptureMode(ClosedString):
    WINDOW = "window"


class InputRoute(ClosedString):
    SEMANTIC = "semantic"
    FOREGROUND = "foreground"


class ActionKind(ClosedString):
    FOCUS_WINDOW = "focus_window"
    FOCUS_ELEMENT = "focus_element"
    INVOKE_ELEMENT = "invoke_element"
    CLICK_POINT = "click_point"
    SET_VALUE = "set_value"
    TOGGLE_ELEMENT = "toggle_element"
    SELECT_ELEMENT = "select_element"
    SET_EXPANDED = "set_expanded"
    MOVE_POINTER = "move_pointer"
    TYPE_TEXT = "type_text"
    PRESS_KEYS = "press_keys"
    SCROLL = "scroll"
    DRAG = "drag"


class PermissionState(ClosedString):
    GRANTED = "granted"
    DENIED = "denied"
    NOT_DETERMINED = "not_determined"
    NOT_APPLICABLE = "not_applicable"
    UNKNOWN = "unknown"


class PermissionMode(ClosedString):
    READ_ONLY = "read_only"
    BOUNDED = "bounded"


class AccessibilityMode(ClosedString):
    DISABLED = "disabled"
    INTERACTIVE = "interactive"
    FULL = "full"


class PointerButton(ClosedString):
    LEFT = "left"
    MIDDLE = "middle"
    RIGHT = "right"


class DeliveryMode(ClosedString):
    SEMANTIC = "semantic"
    FOREGROUND = "foreground"


class SignatureStatus(ClosedString):
    VERIFIED = "verified"
    INVALID = "invalid"
    UNSIGNED = "unsigned"
    UNKNOWN = "unknown"


class MutationStatus(ClosedString):
    NOT_APPLICABLE = "not_applicable"
    NOT_DISPATCHED = "not_dispatched"
    INDETERMINATE = "indeterminate"


class ErrorCode(ClosedString):
    PROTOCOL_MISMATCH = "protocol_mismatch"
    UNAUTHORIZED = "unauthorized"
    INVALID_REQUEST = "invalid_request"
    BUSY = "busy"
    DEADLINE_EXCEEDED = "deadline_exceeded"
    SESSION_UNAVAILABLE = "session_unavailable"
    STALE_DISCOVERY = "stale_discovery"
    CAPABILITY_DENIED = "capability_denied"
    REFERENCE_NOT_FOUND = "reference_not_found"
    STALE_OBSERVATION = "stale_observation"
    PERMISSION_REQUIRED = "permission_required"
    UNSUPPORTED = "unsupported"
    FOREGROUND_REQUIRED = "foreground_required"
    TARGET_UNAVAILABLE = "target_unavailable"
    TARGET_UNRESPONSIVE = "target_unresponsive"
    DRIVER_FAILURE = "driver_failure"
    INTERNAL = "internal"


class TruncationReason(ClosedString):
    NODE_LIMIT = "node_limit"
    DEPTH_LIMIT = "depth_limit"
    BYTE_LIMIT = "byte_limit"
    DEADLINE = "deadline"
    PROVIDER_FAILURE = "provider_failure"


class CUAError(Exception):
    def __init__(
        self,
        code: ErrorCode,
        message: str,
        retryable: bool,
        recovery_action: str | None,
        mutation_status: MutationStatus,
    ) -> None:
        super().__init__(f"nexus-cua: {code.value}: {message}")
        self.code = code
        self.message = message
        self.retryable = retryable
        self.recovery_action = recovery_action
        self.mutation_status = mutation_status

    @classmethod
    def from_wire(cls, value: Any) -> CUAError:
        data = closed_object(
            value,
            {"code", "message", "retryable", "recovery_action", "mutation_status"},
        )
        return cls(
            enum_value(ErrorCode, data["code"]),
            string_value(data["message"]),
            bool_value(data["retryable"]),
            optional_string(data["recovery_action"]),
            enum_value(MutationStatus, data["mutation_status"]),
        )


@dataclass(frozen=True)
class DriverCapabilities:
    protocol_version: str
    runtime_version: str
    platform: Platform
    capture_modes: tuple[CaptureMode, ...]
    accessibility_tree: bool
    input_routes: tuple[InputRoute, ...]
    actions: tuple[ActionKind, ...]

    @classmethod
    def from_wire(cls, value: Any) -> DriverCapabilities:
        data = closed_object(
            value,
            {
                "protocol_version",
                "runtime_version",
                "platform",
                "capture_modes",
                "accessibility_tree",
                "input_routes",
                "actions",
            },
        )
        return cls(
            string_value(data["protocol_version"]),
            string_value(data["runtime_version"]),
            enum_value(Platform, data["platform"]),
            enum_tuple(CaptureMode, data["capture_modes"]),
            bool_value(data["accessibility_tree"]),
            enum_tuple(InputRoute, data["input_routes"]),
            enum_tuple(ActionKind, data["actions"]),
        )


@dataclass(frozen=True)
class PermissionStatus:
    screen_capture: PermissionState
    accessibility: PermissionState
    input_control: PermissionState

    @classmethod
    def from_wire(cls, value: Any) -> PermissionStatus:
        data = closed_object(
            value, {"screen_capture", "accessibility", "input_control"}
        )
        return cls(
            *(
                enum_value(PermissionState, data[name])
                for name in ("screen_capture", "accessibility", "input_control")
            )
        )


@dataclass(frozen=True)
class ApplicationProvenance:
    platform: Platform
    executable_path: str | None
    bundle_id: str | None = None
    signing_team_id: str | None = None
    designated_requirement: str | None = None
    publisher: str | None = None
    signature_status: SignatureStatus | None = None

    @classmethod
    def from_wire(cls, value: Any) -> ApplicationProvenance:
        if not isinstance(value, Mapping):
            raise ValueError("provenance must be an object")
        platform = enum_value(Platform, value.get("platform"))
        if platform is Platform.MACOS:
            data = closed_object(
                value,
                {
                    "platform",
                    "bundle_id",
                    "executable_path",
                    "signing_team_id",
                    "designated_requirement",
                },
            )
            return cls(
                platform,
                optional_string(data["executable_path"]),
                optional_string(data["bundle_id"]),
                optional_string(data["signing_team_id"]),
                optional_string(data["designated_requirement"]),
            )
        if platform is Platform.WINDOWS:
            data = closed_object(
                value, {"platform", "executable_path", "publisher", "signature_status"}
            )
            return cls(
                platform,
                string_value(data["executable_path"]),
                publisher=optional_string(data["publisher"]),
                signature_status=enum_value(SignatureStatus, data["signature_status"]),
            )
        data = closed_object(value, {"platform", "executable_path"})
        return cls(platform, optional_string(data["executable_path"]))


@dataclass(frozen=True)
class DiscoveredApplication:
    discovery_ref: DiscoveryRef
    name: str
    application_id: str
    foreground: bool
    provenance: ApplicationProvenance
    expires_at: str

    @classmethod
    def from_wire(cls, value: Any) -> DiscoveredApplication:
        data = closed_object(
            value,
            {
                "discovery_ref",
                "name",
                "application_id",
                "foreground",
                "provenance",
                "expires_at",
            },
        )
        return cls(
            DiscoveryRef(string_value(data["discovery_ref"])),
            string_value(data["name"]),
            string_value(data["application_id"]),
            bool_value(data["foreground"]),
            ApplicationProvenance.from_wire(data["provenance"]),
            string_value(data["expires_at"]),
        )


@dataclass(frozen=True)
class DiscoverApplicationsOutput:
    applications: tuple[DiscoveredApplication, ...]
    complete: bool

    @classmethod
    def from_wire(cls, value: Any) -> DiscoverApplicationsOutput:
        data = closed_object(value, {"applications", "complete"})
        return cls(
            tuple(
                DiscoveredApplication.from_wire(item)
                for item in list_value(data["applications"])
            ),
            bool_value(data["complete"]),
        )


@dataclass(frozen=True)
class CapabilityManifest:
    mode: PermissionMode
    application_refs: tuple[DiscoveryRef, ...]
    allowed_actions: tuple[ActionKind, ...] = ()
    allow_foreground_input: bool = False
    ttl_seconds: int = 60

    def to_wire(self) -> dict[str, Any]:
        if not self.application_refs:
            raise ValueError("at least one discovery reference is required")
        if self.ttl_seconds <= 0 or self.ttl_seconds > 2**32 - 1:
            raise ValueError("session TTL must be finite and fit u32")
        if self.mode is PermissionMode.READ_ONLY and (
            self.allowed_actions or self.allow_foreground_input
        ):
            raise ValueError("read-only sessions cannot grant mutation authority")
        return {
            "mode": self.mode.value,
            "application_refs": list(self.application_refs),
            "allowed_actions": [action.value for action in self.allowed_actions],
            "allow_foreground_input": self.allow_foreground_input,
            "ttl_seconds": self.ttl_seconds,
        }


@dataclass(frozen=True)
class OpenSessionOutput:
    session_id: SessionID
    expires_at: str

    @classmethod
    def from_wire(cls, value: Any) -> OpenSessionOutput:
        data = closed_object(value, {"session_id", "expires_at"})
        return cls(
            SessionID(string_value(data["session_id"])),
            string_value(data["expires_at"]),
        )


@dataclass(frozen=True)
class ApplicationSummary:
    app_ref: AppRef
    name: str
    application_id: str
    foreground: bool

    @classmethod
    def from_wire(cls, value: Any) -> ApplicationSummary:
        data = closed_object(value, {"app_ref", "name", "application_id", "foreground"})
        return cls(
            AppRef(string_value(data["app_ref"])),
            string_value(data["name"]),
            string_value(data["application_id"]),
            bool_value(data["foreground"]),
        )


@dataclass(frozen=True)
class ScreenRect:
    x: float
    y: float
    width: float
    height: float

    @classmethod
    def from_wire(cls, value: Any) -> ScreenRect:
        data = closed_object(value, {"x", "y", "width", "height"})
        return cls(
            *(number_value(data[name]) for name in ("x", "y", "width", "height"))
        )

    def to_wire(self) -> dict[str, float]:
        if not all(
            math.isfinite(value) for value in (self.x, self.y, self.width, self.height)
        ):
            raise ValueError("screen rectangle values must be finite")
        return {"x": self.x, "y": self.y, "width": self.width, "height": self.height}


@dataclass(frozen=True)
class ScreenshotPoint:
    x: int
    y: int

    def to_wire(self) -> dict[str, int]:
        if not (0 <= self.x <= 2**32 - 1 and 0 <= self.y <= 2**32 - 1):
            raise ValueError("screenshot point must fit unsigned 32-bit coordinates")
        return {"x": self.x, "y": self.y}


@dataclass(frozen=True)
class PixelSize:
    width: int
    height: int

    @classmethod
    def from_wire(cls, value: Any) -> PixelSize:
        data = closed_object(value, {"width", "height"})
        return cls(integer_value(data["width"]), integer_value(data["height"]))


@dataclass(frozen=True)
class ScreenshotMapping:
    screen_bounds: ScreenRect
    pixel_size: PixelSize

    @classmethod
    def from_wire(cls, value: Any) -> ScreenshotMapping:
        data = closed_object(value, {"screen_bounds", "pixel_size"})
        return cls(
            ScreenRect.from_wire(data["screen_bounds"]),
            PixelSize.from_wire(data["pixel_size"]),
        )


@dataclass(frozen=True)
class WindowSummary:
    window_ref: WindowRef
    app_ref: AppRef
    title: str
    screen_bounds: ScreenRect
    minimized: bool
    visible: bool
    foreground: bool

    @classmethod
    def from_wire(cls, value: Any) -> WindowSummary:
        data = closed_object(
            value,
            {
                "window_ref",
                "app_ref",
                "title",
                "screen_bounds",
                "minimized",
                "visible",
                "foreground",
            },
        )
        return cls(
            WindowRef(string_value(data["window_ref"])),
            AppRef(string_value(data["app_ref"])),
            string_value(data["title"]),
            ScreenRect.from_wire(data["screen_bounds"]),
            bool_value(data["minimized"]),
            bool_value(data["visible"]),
            bool_value(data["foreground"]),
        )


@dataclass(frozen=True)
class ScreenshotArtifact:
    artifact_ref: ArtifactRef
    path: str
    mime_type: str
    mapping: ScreenshotMapping
    byte_length: int
    sha256: str

    @classmethod
    def from_wire(cls, value: Any) -> ScreenshotArtifact:
        data = closed_object(
            value,
            {"artifact_ref", "path", "mime_type", "mapping", "byte_length", "sha256"},
        )
        return cls(
            ArtifactRef(string_value(data["artifact_ref"])),
            string_value(data["path"]),
            string_value(data["mime_type"]),
            ScreenshotMapping.from_wire(data["mapping"]),
            integer_value(data["byte_length"]),
            string_value(data["sha256"]),
        )


@dataclass(frozen=True)
class AccessibilityElement:
    element_ref: ElementRef
    parent_ref: ElementRef | None
    role: str
    name: str
    value: str | None
    screen_bounds: ScreenRect | None
    enabled: bool
    focused: bool
    actions: tuple[str, ...]

    @classmethod
    def from_wire(cls, value: Any) -> AccessibilityElement:
        data = closed_object(
            value,
            {
                "element_ref",
                "parent_ref",
                "role",
                "name",
                "value",
                "screen_bounds",
                "enabled",
                "focused",
                "actions",
            },
        )
        bounds = (
            None
            if data["screen_bounds"] is None
            else ScreenRect.from_wire(data["screen_bounds"])
        )
        return cls(
            ElementRef(string_value(data["element_ref"])),
            optional_newtype(ElementRef, data["parent_ref"]),
            string_value(data["role"]),
            string_value(data["name"]),
            optional_string(data["value"]),
            bounds,
            bool_value(data["enabled"]),
            bool_value(data["focused"]),
            tuple(string_value(item) for item in list_value(data["actions"])),
        )


@dataclass(frozen=True)
class ObservationTruncation:
    reason: TruncationReason
    emitted_elements: int

    @classmethod
    def from_wire(cls, value: Any) -> ObservationTruncation:
        data = closed_object(value, {"reason", "emitted_elements"})
        return cls(
            enum_value(TruncationReason, data["reason"]),
            integer_value(data["emitted_elements"]),
        )


@dataclass(frozen=True)
class WindowObservation:
    observation_id: ObservationID
    window_ref: WindowRef
    captured_at: str
    window_screen_bounds: ScreenRect
    screenshot: ScreenshotArtifact | None
    elements: tuple[AccessibilityElement, ...]
    elements_complete: bool
    elements_truncation: ObservationTruncation | None

    @classmethod
    def from_wire(cls, value: Any) -> WindowObservation:
        data = closed_object(
            value,
            {
                "observation_id",
                "window_ref",
                "captured_at",
                "window_screen_bounds",
                "screenshot",
                "elements",
                "elements_complete",
                "elements_truncation",
            },
        )
        screenshot = (
            None
            if data["screenshot"] is None
            else ScreenshotArtifact.from_wire(data["screenshot"])
        )
        truncation = (
            None
            if data["elements_truncation"] is None
            else ObservationTruncation.from_wire(data["elements_truncation"])
        )
        return cls(
            ObservationID(string_value(data["observation_id"])),
            WindowRef(string_value(data["window_ref"])),
            string_value(data["captured_at"]),
            ScreenRect.from_wire(data["window_screen_bounds"]),
            screenshot,
            tuple(
                AccessibilityElement.from_wire(item)
                for item in list_value(data["elements"])
            ),
            bool_value(data["elements_complete"]),
            truncation,
        )


@dataclass(frozen=True)
class ActionOutput:
    delivery_mode: DeliveryMode
    dispatched: bool
    observation_invalidated: bool

    @classmethod
    def from_wire(cls, value: Any) -> ActionOutput:
        data = closed_object(
            value, {"delivery_mode", "dispatched", "observation_invalidated"}
        )
        return cls(
            enum_value(DeliveryMode, data["delivery_mode"]),
            bool_value(data["dispatched"]),
            bool_value(data["observation_invalidated"]),
        )


@dataclass(frozen=True)
class VerificationOutput:
    matched: bool
    evidence: str

    @classmethod
    def from_wire(cls, value: Any) -> VerificationOutput:
        data = closed_object(value, {"matched", "evidence"})
        return cls(bool_value(data["matched"]), string_value(data["evidence"]))


class SensitiveText:
    __slots__ = ("_value",)

    def __init__(self, value: str) -> None:
        self._value = value

    def __str__(self) -> str:
        return "[REDACTED]"

    def __repr__(self) -> str:
        return "SensitiveText([REDACTED])"

    def _wire_value(self) -> str:
        return self._value


class Action:
    def to_wire(self) -> dict[str, Any]:
        raise NotImplementedError


@dataclass(frozen=True)
class FocusWindow(Action):
    def to_wire(self) -> dict[str, Any]:
        return {"kind": "focus_window"}


@dataclass(frozen=True)
class FocusElement(Action):
    element_ref: ElementRef

    def to_wire(self) -> dict[str, Any]:
        return {"kind": "focus_element", "element_ref": self.element_ref}


@dataclass(frozen=True)
class InvokeElement(Action):
    element_ref: ElementRef

    def to_wire(self) -> dict[str, Any]:
        return {"kind": "invoke_element", "element_ref": self.element_ref}


@dataclass(frozen=True)
class ClickPoint(Action):
    point: ScreenshotPoint
    button: PointerButton = PointerButton.LEFT
    count: int = 1

    def to_wire(self) -> dict[str, Any]:
        return {
            "kind": "click_point",
            "point": self.point.to_wire(),
            "button": self.button.value,
            "count": self.count,
        }


@dataclass(frozen=True)
class SetValue(Action):
    element_ref: ElementRef
    value: SensitiveText

    def to_wire(self) -> dict[str, Any]:
        return {
            "kind": "set_value",
            "element_ref": self.element_ref,
            "value": self.value._wire_value(),
        }


@dataclass(frozen=True)
class ToggleElement(Action):
    element_ref: ElementRef

    def to_wire(self) -> dict[str, Any]:
        return {"kind": "toggle_element", "element_ref": self.element_ref}


@dataclass(frozen=True)
class SelectElement(Action):
    element_ref: ElementRef

    def to_wire(self) -> dict[str, Any]:
        return {"kind": "select_element", "element_ref": self.element_ref}


@dataclass(frozen=True)
class SetExpanded(Action):
    element_ref: ElementRef
    expanded: bool

    def to_wire(self) -> dict[str, Any]:
        return {
            "kind": "set_expanded",
            "element_ref": self.element_ref,
            "expanded": self.expanded,
        }


@dataclass(frozen=True)
class MovePointer(Action):
    point: ScreenshotPoint
    duration_ms: int

    def to_wire(self) -> dict[str, Any]:
        return {
            "kind": "move_pointer",
            "point": self.point.to_wire(),
            "duration_ms": self.duration_ms,
        }


@dataclass(frozen=True)
class TypeText(Action):
    text: SensitiveText

    def to_wire(self) -> dict[str, Any]:
        return {"kind": "type_text", "text": self.text._wire_value()}


@dataclass(frozen=True)
class PressKeys(Action):
    keys: tuple[str, ...]

    def to_wire(self) -> dict[str, Any]:
        return {"kind": "press_keys", "keys": list(self.keys)}


@dataclass(frozen=True)
class Scroll(Action):
    delta_x: float
    delta_y: float
    element_ref: ElementRef | None = None

    def to_wire(self) -> dict[str, Any]:
        return {
            "kind": "scroll",
            "element_ref": self.element_ref,
            "delta_x": self.delta_x,
            "delta_y": self.delta_y,
        }


@dataclass(frozen=True)
class Drag(Action):
    from_point: ScreenshotPoint
    to_point: ScreenshotPoint
    duration_ms: int

    def to_wire(self) -> dict[str, Any]:
        return {
            "kind": "drag",
            "from": self.from_point.to_wire(),
            "to": self.to_point.to_wire(),
            "duration_ms": self.duration_ms,
        }


class StatePredicate:
    def to_wire(self) -> dict[str, Any]:
        raise NotImplementedError


@dataclass(frozen=True)
class WindowTitleContains(StatePredicate):
    text: str

    def to_wire(self) -> dict[str, Any]:
        return {"kind": "window_title_contains", "text": self.text}


@dataclass(frozen=True)
class ElementExists(StatePredicate):
    role: str | None = None
    name: str | None = None

    def to_wire(self) -> dict[str, Any]:
        return {"kind": "element_exists", "role": self.role, "name": self.name}


@dataclass(frozen=True)
class BoundsContained(StatePredicate):
    inner: ScreenRect
    outer: ScreenRect

    def to_wire(self) -> dict[str, Any]:
        return {
            "kind": "bounds_contained",
            "inner": self.inner.to_wire(),
            "outer": self.outer.to_wire(),
        }


_ACTION_TYPES = (
    FocusWindow,
    FocusElement,
    InvokeElement,
    ClickPoint,
    SetValue,
    ToggleElement,
    SelectElement,
    SetExpanded,
    MovePointer,
    TypeText,
    PressKeys,
    Scroll,
    Drag,
)
_PREDICATE_TYPES = (WindowTitleContains, ElementExists, BoundsContained)


def action_wire(value: Action) -> dict[str, Any]:
    if type(value) not in _ACTION_TYPES:
        raise TypeError(f"unsupported action type {type(value).__name__}")
    if isinstance(value, ClickPoint) and not 1 <= value.count <= 255:
        raise ValueError("click count must fit a non-zero u8")
    if (
        isinstance(value, (MovePointer, Drag))
        and not 0 <= value.duration_ms <= 2**32 - 1
    ):
        raise ValueError("action duration must fit u32")
    if isinstance(value, PressKeys) and (
        not value.keys or any(not isinstance(key, str) or not key for key in value.keys)
    ):
        raise ValueError("key chord must contain non-empty strings")
    if isinstance(value, Scroll) and not all(
        math.isfinite(item) for item in (value.delta_x, value.delta_y)
    ):
        raise ValueError("scroll deltas must be finite")
    if isinstance(value, (SetValue, TypeText)):
        secret = value.value if isinstance(value, SetValue) else value.text
        if not isinstance(secret, SensitiveText):
            raise TypeError("sensitive action text must use SensitiveText")
    return value.to_wire()


def predicate_wire(value: StatePredicate) -> dict[str, Any]:
    if type(value) not in _PREDICATE_TYPES:
        raise TypeError(f"unsupported predicate type {type(value).__name__}")
    return value.to_wire()


E = TypeVar("E", bound=ClosedString)


def closed_object(value: Any, required: set[str]) -> dict[str, Any]:
    if not isinstance(value, Mapping):
        raise ValueError("expected an object")
    keys = set(value.keys())
    if keys != required:
        missing = required - keys
        unknown = keys - required
        raise ValueError(
            f"object shape mismatch; missing={sorted(missing)} unknown={sorted(unknown)}"
        )
    return dict(value)


def enum_value(kind: type[E], value: Any) -> E:
    if not isinstance(value, str):
        raise ValueError(f"expected {kind.__name__} string")
    try:
        return kind(value)
    except ValueError as error:
        raise ValueError(f"unknown {kind.__name__} {value!r}") from error


def enum_tuple(kind: type[E], value: Any) -> tuple[E, ...]:
    return tuple(enum_value(kind, item) for item in list_value(value))


def list_value(value: Any) -> list[Any]:
    if not isinstance(value, list):
        raise ValueError("expected a list")
    return value


def string_value(value: Any) -> str:
    if not isinstance(value, str):
        raise ValueError("expected a string")
    return value


def optional_string(value: Any) -> str | None:
    return None if value is None else string_value(value)


def optional_newtype(kind: Any, value: Any) -> Any:
    return None if value is None else kind(string_value(value))


def bool_value(value: Any) -> bool:
    if not isinstance(value, bool):
        raise ValueError("expected a boolean")
    return value


def integer_value(value: Any) -> int:
    if not isinstance(value, int) or isinstance(value, bool):
        raise ValueError("expected an integer")
    return value


def number_value(value: Any) -> float:
    if not isinstance(value, (int, float)) or isinstance(value, bool):
        raise ValueError("expected a number")
    return float(value)
