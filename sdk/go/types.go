// Package nexuscua is the official Go client for the private Nexus Computer
// Use Runtime sidecar protocol.
package nexuscua

import (
	"encoding/json"
	"fmt"
)

const ProtocolVersion = "nexus.cua.v1"

type RequestID string
type SessionID string
type AppRef string
type DiscoveryRef string
type LaunchRef string
type WindowRef string
type DisplayRef string
type ObservationID string
type ElementRef string
type ArtifactRef string
type ClipboardRef string

type Platform string

const (
	PlatformMacOS       Platform = "macos"
	PlatformWindows     Platform = "windows"
	PlatformUnsupported Platform = "unsupported"
)

type CaptureMode string

const (
	CaptureModeWindow  CaptureMode = "window"
	CaptureModeDisplay CaptureMode = "display"
)

type InputRoute string

const (
	InputRouteSemantic   InputRoute = "semantic"
	InputRouteSystem     InputRoute = "system"
	InputRouteBackground InputRoute = "background"
	InputRouteForeground InputRoute = "foreground"
)

type ApplicationLifecycleOperation string

const ApplicationLifecycleLaunch ApplicationLifecycleOperation = "launch"

type ClipboardOperation string

const (
	ClipboardReadText  ClipboardOperation = "read_text"
	ClipboardWriteText ClipboardOperation = "write_text"
)

type DeliveryPreference string

const (
	DeliveryPreferenceBackground DeliveryPreference = "background"
	DeliveryPreferenceForeground DeliveryPreference = "foreground"
)

type ActionKind string

const (
	ActionFocusWindow          ActionKind = "focus_window"
	ActionSetWindowBounds      ActionKind = "set_window_bounds"
	ActionSetWindowMinimized   ActionKind = "set_window_minimized"
	ActionTerminateApplication ActionKind = "terminate_application"
	ActionFocusElement         ActionKind = "focus_element"
	ActionInvokeElement        ActionKind = "invoke_element"
	ActionClickPoint           ActionKind = "click_point"
	ActionPressPoint           ActionKind = "press_point"
	ActionSetValue             ActionKind = "set_value"
	ActionSetRangeValue        ActionKind = "set_range_value"
	ActionToggleElement        ActionKind = "toggle_element"
	ActionSelectElement        ActionKind = "select_element"
	ActionSetExpanded          ActionKind = "set_expanded"
	ActionMovePointer          ActionKind = "move_pointer"
	ActionTypeText             ActionKind = "type_text"
	ActionPressKeys            ActionKind = "press_keys"
	ActionScroll               ActionKind = "scroll"
	ActionDrag                 ActionKind = "drag"
)

type ElementState string

const (
	ElementStateEnabled  ElementState = "enabled"
	ElementStateFocused  ElementState = "focused"
	ElementStateSelected ElementState = "selected"
	ElementStateChecked  ElementState = "checked"
	ElementStateExpanded ElementState = "expanded"
)

type PermissionState string

const (
	PermissionGranted       PermissionState = "granted"
	PermissionDenied        PermissionState = "denied"
	PermissionNotDetermined PermissionState = "not_determined"
	PermissionNotApplicable PermissionState = "not_applicable"
	PermissionUnknown       PermissionState = "unknown"
)

type PermissionMode string

const (
	PermissionReadOnly PermissionMode = "read_only"
	PermissionBounded  PermissionMode = "bounded"
)

type AccessibilityMode string

const (
	AccessibilityDisabled    AccessibilityMode = "disabled"
	AccessibilityInteractive AccessibilityMode = "interactive"
	AccessibilityFull        AccessibilityMode = "full"
)

type PointerButton string

const (
	PointerLeft   PointerButton = "left"
	PointerMiddle PointerButton = "middle"
	PointerRight  PointerButton = "right"
)

type DeliveryMode string

const (
	DeliverySemantic   DeliveryMode = "semantic"
	DeliverySystem     DeliveryMode = "system"
	DeliveryBackground DeliveryMode = "background"
	DeliveryForeground DeliveryMode = "foreground"
)

type ActionEffectStatus string

const (
	ActionEffectConfirmed  ActionEffectStatus = "confirmed"
	ActionEffectUnverified ActionEffectStatus = "unverified"
)

type ActionEvidenceKind string

const (
	ActionEvidenceNativeReadback     ActionEvidenceKind = "native_readback"
	ActionEvidenceDispatchAcceptance ActionEvidenceKind = "dispatch_acceptance"
)

type ForegroundRestorationStatus string

const (
	ForegroundRestorationNotRequested        ForegroundRestorationStatus = "not_requested"
	ForegroundRestorationNotNeeded           ForegroundRestorationStatus = "not_needed"
	ForegroundRestorationRestored            ForegroundRestorationStatus = "restored"
	ForegroundRestorationSkippedOwnerChanged ForegroundRestorationStatus = "skipped_owner_changed"
	ForegroundRestorationTargetUnavailable   ForegroundRestorationStatus = "target_unavailable"
	ForegroundRestorationFailed              ForegroundRestorationStatus = "failed"
)

type SignatureStatus string

const (
	SignatureVerified SignatureStatus = "verified"
	SignatureInvalid  SignatureStatus = "invalid"
	SignatureUnsigned SignatureStatus = "unsigned"
	SignatureUnknown  SignatureStatus = "unknown"
)

type MutationStatus string

const (
	MutationNotApplicable MutationStatus = "not_applicable"
	MutationNotDispatched MutationStatus = "not_dispatched"
	MutationIndeterminate MutationStatus = "indeterminate"
)

type MutationAdmission string

const (
	MutationAdmissionUnknown  MutationAdmission = "unknown"
	MutationAdmissionAdmitted MutationAdmission = "admitted"
)

type MutationDisposition string

const (
	MutationDispositionUnknown       MutationDisposition = "unknown"
	MutationDispositionNotMutation   MutationDisposition = "not_mutation"
	MutationDispositionPending       MutationDisposition = "pending"
	MutationDispositionSucceeded     MutationDisposition = "succeeded"
	MutationDispositionNotDispatched MutationDisposition = "not_dispatched"
	MutationDispositionIndeterminate MutationDisposition = "indeterminate"
)

type ErrorCode string

const (
	ErrorProtocolMismatch   ErrorCode = "protocol_mismatch"
	ErrorUnauthorized       ErrorCode = "unauthorized"
	ErrorInvalidRequest     ErrorCode = "invalid_request"
	ErrorBusy               ErrorCode = "busy"
	ErrorDeadlineExceeded   ErrorCode = "deadline_exceeded"
	ErrorSessionUnavailable ErrorCode = "session_unavailable"
	ErrorStaleDiscovery     ErrorCode = "stale_discovery"
	ErrorCapabilityDenied   ErrorCode = "capability_denied"
	ErrorReferenceNotFound  ErrorCode = "reference_not_found"
	ErrorStaleObservation   ErrorCode = "stale_observation"
	ErrorStaleClipboard     ErrorCode = "stale_clipboard"
	ErrorPermissionRequired ErrorCode = "permission_required"
	ErrorUnsupported        ErrorCode = "unsupported"
	ErrorForegroundRequired ErrorCode = "foreground_required"
	ErrorTargetUnavailable  ErrorCode = "target_unavailable"
	ErrorTargetUnresponsive ErrorCode = "target_unresponsive"
	ErrorDriverFailure      ErrorCode = "driver_failure"
	ErrorInternal           ErrorCode = "internal"
)

type TruncationReason string

const (
	TruncationNodeLimit       TruncationReason = "node_limit"
	TruncationDepthLimit      TruncationReason = "depth_limit"
	TruncationByteLimit       TruncationReason = "byte_limit"
	TruncationDeadline        TruncationReason = "deadline"
	TruncationProviderFailure TruncationReason = "provider_failure"
)

type CUAError struct {
	Code           ErrorCode      `json:"code"`
	Message        string         `json:"message"`
	Retryable      bool           `json:"retryable"`
	RecoveryAction *string        `json:"recovery_action"`
	MutationStatus MutationStatus `json:"mutation_status"`
}

func (e *CUAError) Error() string {
	return fmt.Sprintf("nexus-cua: %s: %s", e.Code, e.Message)
}

type DriverCapabilities struct {
	ProtocolVersion      string                          `json:"protocol_version"`
	RuntimeVersion       string                          `json:"runtime_version"`
	Platform             Platform                        `json:"platform"`
	CaptureModes         []CaptureMode                   `json:"capture_modes"`
	AccessibilityTree    bool                            `json:"accessibility_tree"`
	InputRoutes          []InputRoute                    `json:"input_routes"`
	ApplicationLifecycle []ApplicationLifecycleOperation `json:"application_lifecycle"`
	Clipboard            []ClipboardOperation            `json:"clipboard"`
	Actions              []ActionKind                    `json:"actions"`
}

type PermissionStatus struct {
	ScreenCapture PermissionState `json:"screen_capture"`
	Accessibility PermissionState `json:"accessibility"`
	InputControl  PermissionState `json:"input_control"`
}

type ApplicationProvenance struct {
	Platform              Platform         `json:"platform"`
	BundleID              *string          `json:"bundle_id,omitempty"`
	ExecutablePath        *string          `json:"executable_path"`
	SigningTeamID         *string          `json:"signing_team_id,omitempty"`
	DesignatedRequirement *string          `json:"designated_requirement,omitempty"`
	Publisher             *string          `json:"publisher,omitempty"`
	SignatureStatus       *SignatureStatus `json:"signature_status,omitempty"`
}

type DiscoveredApplication struct {
	DiscoveryRef  DiscoveryRef          `json:"discovery_ref"`
	Name          string                `json:"name"`
	ApplicationID string                `json:"application_id"`
	Foreground    bool                  `json:"foreground"`
	Provenance    ApplicationProvenance `json:"provenance"`
	ExpiresAt     string                `json:"expires_at"`
}

type DiscoverApplicationsOutput struct {
	Applications []DiscoveredApplication `json:"applications"`
	Complete     bool                    `json:"complete"`
}

type LaunchableApplication struct {
	LaunchRef     LaunchRef             `json:"launch_ref"`
	Name          string                `json:"name"`
	ApplicationID string                `json:"application_id"`
	Provenance    ApplicationProvenance `json:"provenance"`
	ExpiresAt     string                `json:"expires_at"`
}

type DiscoverLaunchableApplicationsOutput struct {
	Applications []LaunchableApplication `json:"applications"`
	Complete     bool                    `json:"complete"`
}

type LaunchApplicationInput struct {
	LaunchRef                 LaunchRef `json:"launch_ref"`
	AllowForegroundActivation bool      `json:"allow_foreground_activation"`
}

type ApplicationLaunchOutput struct {
	Application  DiscoveredApplication `json:"application"`
	DeliveryMode DeliveryMode          `json:"delivery_mode"`
	Dispatched   bool                  `json:"dispatched"`
}

type CapabilityManifest struct {
	Mode                        PermissionMode `json:"mode"`
	ApplicationRefs             []DiscoveryRef `json:"application_refs"`
	AllowedActions              []ActionKind   `json:"allowed_actions"`
	AllowForegroundInput        bool           `json:"allow_foreground_input"`
	RestoreForegroundAfterInput bool           `json:"restore_foreground_after_input"`
	AllowDisplayCapture         bool           `json:"allow_display_capture"`
	AllowClipboardRead          bool           `json:"allow_clipboard_read"`
	AllowClipboardWrite         bool           `json:"allow_clipboard_write"`
	TTLSeconds                  uint32         `json:"ttl_seconds"`
}

type OpenSessionInput struct {
	Manifest CapabilityManifest `json:"manifest"`
}

type OpenSessionOutput struct {
	SessionID SessionID `json:"session_id"`
	ExpiresAt string    `json:"expires_at"`
}

type ApplicationSummary struct {
	AppRef        AppRef `json:"app_ref"`
	Name          string `json:"name"`
	ApplicationID string `json:"application_id"`
	Foreground    bool   `json:"foreground"`
}

type ScreenRect struct {
	X      float64 `json:"x"`
	Y      float64 `json:"y"`
	Width  float64 `json:"width"`
	Height float64 `json:"height"`
}

type ScreenshotPoint struct {
	X uint32 `json:"x"`
	Y uint32 `json:"y"`
}

type PixelSize struct {
	Width  uint32 `json:"width"`
	Height uint32 `json:"height"`
}

type ScreenshotMapping struct {
	ScreenBounds ScreenRect `json:"screen_bounds"`
	PixelSize    PixelSize  `json:"pixel_size"`
}

type WindowSummary struct {
	WindowRef    WindowRef  `json:"window_ref"`
	AppRef       AppRef     `json:"app_ref"`
	Title        string     `json:"title"`
	ScreenBounds ScreenRect `json:"screen_bounds"`
	Minimized    bool       `json:"minimized"`
	Visible      bool       `json:"visible"`
	Foreground   bool       `json:"foreground"`
}

type DisplaySummary struct {
	DisplayRef   DisplayRef `json:"display_ref"`
	ScreenBounds ScreenRect `json:"screen_bounds"`
	Primary      bool       `json:"primary"`
}

type ObserveDisplayInput struct {
	SessionID  SessionID  `json:"session_id"`
	DisplayRef DisplayRef `json:"display_ref"`
}

type ObserveWindowInput struct {
	SessionID         SessionID         `json:"session_id"`
	WindowRef         WindowRef         `json:"window_ref"`
	IncludeScreenshot bool              `json:"include_screenshot"`
	Accessibility     AccessibilityMode `json:"accessibility"`
}

type ScreenshotArtifact struct {
	ArtifactRef ArtifactRef       `json:"artifact_ref"`
	Path        string            `json:"path"`
	MIMEType    string            `json:"mime_type"`
	Mapping     ScreenshotMapping `json:"mapping"`
	ByteLength  uint64            `json:"byte_length"`
	SHA256      string            `json:"sha256"`
}

type AccessibilityElement struct {
	ElementRef   ElementRef               `json:"element_ref"`
	ParentRef    *ElementRef              `json:"parent_ref"`
	Role         string                   `json:"role"`
	Name         string                   `json:"name"`
	Value        *string                  `json:"value"`
	RangeValue   *AccessibilityRangeValue `json:"range_value"`
	ScreenBounds *ScreenRect              `json:"screen_bounds"`
	Enabled      bool                     `json:"enabled"`
	Focused      bool                     `json:"focused"`
	Selected     *bool                    `json:"selected"`
	Checked      *bool                    `json:"checked"`
	Expanded     *bool                    `json:"expanded"`
	Actions      []string                 `json:"actions"`
}

type AccessibilityRangeValue struct {
	Value       float64  `json:"value"`
	Minimum     float64  `json:"minimum"`
	Maximum     float64  `json:"maximum"`
	SmallChange *float64 `json:"small_change"`
	LargeChange *float64 `json:"large_change"`
	ReadOnly    bool     `json:"read_only"`
}

type ObservationTruncation struct {
	Reason          TruncationReason `json:"reason"`
	EmittedElements uint32           `json:"emitted_elements"`
}

type WindowObservation struct {
	ObservationID      ObservationID          `json:"observation_id"`
	WindowRef          WindowRef              `json:"window_ref"`
	CapturedAt         string                 `json:"captured_at"`
	WindowScreenBounds ScreenRect             `json:"window_screen_bounds"`
	Screenshot         *ScreenshotArtifact    `json:"screenshot"`
	Elements           []AccessibilityElement `json:"elements"`
	ElementsComplete   bool                   `json:"elements_complete"`
	ElementsTruncation *ObservationTruncation `json:"elements_truncation"`
}

type DisplayObservation struct {
	ObservationID       ObservationID      `json:"observation_id"`
	DisplayRef          DisplayRef         `json:"display_ref"`
	CapturedAt          string             `json:"captured_at"`
	DisplayScreenBounds ScreenRect         `json:"display_screen_bounds"`
	Screenshot          ScreenshotArtifact `json:"screenshot"`
}

type ClipboardSnapshot struct {
	ClipboardRef ClipboardRef   `json:"clipboard_ref"`
	Text         *SensitiveText `json:"text"`
	ByteLength   uint32         `json:"byte_length"`
}

type ClipboardWriteInput struct {
	SessionID    SessionID
	ClipboardRef ClipboardRef
	Text         SensitiveText
}

type ClipboardWriteOutput struct {
	ClipboardRef ClipboardRef       `json:"clipboard_ref"`
	DeliveryMode DeliveryMode       `json:"delivery_mode"`
	Dispatched   bool               `json:"dispatched"`
	EffectStatus ActionEffectStatus `json:"effect_status"`
	EvidenceKind ActionEvidenceKind `json:"evidence_kind"`
}

type ActionOutput struct {
	DeliveryMode           DeliveryMode                `json:"delivery_mode"`
	Dispatched             bool                        `json:"dispatched"`
	EffectStatus           ActionEffectStatus          `json:"effect_status"`
	EvidenceKind           ActionEvidenceKind          `json:"evidence_kind"`
	ForegroundRestoration  ForegroundRestorationStatus `json:"foreground_restoration"`
	ObservationInvalidated bool                        `json:"observation_invalidated"`
}

type MutationReconciliation struct {
	RequestID                RequestID           `json:"request_id"`
	RuntimeEpoch             string              `json:"runtime_epoch"`
	WorkerEpoch              *string             `json:"worker_epoch"`
	Admission                MutationAdmission   `json:"admission"`
	Disposition              MutationDisposition `json:"disposition"`
	Dispatched               *bool               `json:"dispatched"`
	EffectStatus             *ActionEffectStatus `json:"effect_status"`
	EvidenceKind             *ActionEvidenceKind `json:"evidence_kind"`
	ErrorCode                *ErrorCode          `json:"error_code"`
	FreshObservationRequired bool                `json:"fresh_observation_required"`
}

type VerificationOutput struct {
	Matched  bool   `json:"matched"`
	Evidence string `json:"evidence"`
}

// SensitiveText redacts itself from formatting while retaining explicit JSON
// serialization for a bounded action request.
type SensitiveText struct{ value string }

func NewSensitiveText(value string) SensitiveText { return SensitiveText{value: value} }
func (SensitiveText) String() string              { return "[REDACTED]" }
func (SensitiveText) GoString() string            { return "SensitiveText([REDACTED])" }
func (s SensitiveText) Expose() string            { return s.value }

func (s SensitiveText) MarshalJSON() ([]byte, error) { return json.Marshal(s.value) }
func (s *SensitiveText) UnmarshalJSON(data []byte) error {
	return json.Unmarshal(data, &s.value)
}

type Action interface {
	closedAction()
}

type FocusWindow struct{}

func (FocusWindow) closedAction() {}

type SetWindowBounds struct {
	Bounds ScreenRect `json:"bounds"`
}

func (SetWindowBounds) closedAction() {}

type SetWindowMinimized struct {
	Minimized bool `json:"minimized"`
}

func (SetWindowMinimized) closedAction() {}

// TerminateApplication requests a cooperative exit of the observed window's
// exact owning application generation.
type TerminateApplication struct{}

func (TerminateApplication) closedAction() {}

type FocusElement struct {
	ElementRef ElementRef `json:"element_ref"`
}

func (FocusElement) closedAction() {}

type InvokeElement struct {
	ElementRef ElementRef `json:"element_ref"`
}

func (InvokeElement) closedAction() {}

type ClickPoint struct {
	Point    ScreenshotPoint    `json:"point"`
	Button   PointerButton      `json:"button"`
	Count    uint8              `json:"count"`
	Delivery DeliveryPreference `json:"delivery"`
}

func (ClickPoint) closedAction() {}

type PressPoint struct {
	Point      ScreenshotPoint `json:"point"`
	Button     PointerButton   `json:"button"`
	DurationMS uint32          `json:"duration_ms"`
}

func (PressPoint) closedAction() {}

type SetValue struct {
	ElementRef ElementRef    `json:"element_ref"`
	Value      SensitiveText `json:"value"`
}

func (SetValue) closedAction() {}

type SetRangeValue struct {
	ElementRef ElementRef `json:"element_ref"`
	Value      float64    `json:"value"`
}

func (SetRangeValue) closedAction() {}

type ToggleElement struct {
	ElementRef ElementRef `json:"element_ref"`
}

func (ToggleElement) closedAction() {}

type SelectElement struct {
	ElementRef ElementRef `json:"element_ref"`
}

func (SelectElement) closedAction() {}

type SetExpanded struct {
	ElementRef ElementRef `json:"element_ref"`
	Expanded   bool       `json:"expanded"`
}

func (SetExpanded) closedAction() {}

type MovePointer struct {
	Point      ScreenshotPoint `json:"point"`
	DurationMS uint32          `json:"duration_ms"`
}

func (MovePointer) closedAction() {}

type TypeText struct {
	Text SensitiveText `json:"text"`
}

func (TypeText) closedAction() {}

type PressKeys struct {
	Keys []string `json:"keys"`
}

func (PressKeys) closedAction() {}

type Scroll struct {
	ElementRef *ElementRef `json:"element_ref"`
	DeltaX     float64     `json:"delta_x"`
	DeltaY     float64     `json:"delta_y"`
}

func (Scroll) closedAction() {}

type Drag struct {
	From       ScreenshotPoint `json:"from"`
	To         ScreenshotPoint `json:"to"`
	DurationMS uint32          `json:"duration_ms"`
}

func (Drag) closedAction() {}

type StatePredicate interface{ closedPredicate() }

type WindowTitleContains struct {
	Text string `json:"text"`
}

func (WindowTitleContains) closedPredicate() {}

type ElementExists struct {
	Role *string `json:"role"`
	Name *string `json:"name"`
}

func (ElementExists) closedPredicate() {}

type ElementStateMatches struct {
	Role  *string      `json:"role"`
	Name  *string      `json:"name"`
	State ElementState `json:"state"`
	Value bool         `json:"value"`
}

func (ElementStateMatches) closedPredicate() {}

type BoundsContained struct {
	Inner ScreenRect `json:"inner"`
	Outer ScreenRect `json:"outer"`
}

func (BoundsContained) closedPredicate() {}

type PerformActionInput struct {
	SessionID     SessionID
	WindowRef     WindowRef
	ObservationID ObservationID
	Action        Action
}

type VerifyStateInput struct {
	SessionID SessionID
	WindowRef WindowRef
	Predicate StatePredicate
}
