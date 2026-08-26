package nexuscua

import (
	"encoding/json"
	"fmt"
)

func decodeClosedEnum(data []byte, name string, allowed []string) (string, error) {
	var value string
	if err := json.Unmarshal(data, &value); err != nil {
		return "", err
	}
	if !contains(allowed, value) {
		return "", fmt.Errorf("nexus-cua: unknown %s %q", name, value)
	}
	return value, nil
}

func (value *Platform) UnmarshalJSON(data []byte) error {
	parsed, err := decodeClosedEnum(data, "platform", []string{"macos", "windows", "unsupported"})
	*value = Platform(parsed)
	return err
}

func (value *CaptureMode) UnmarshalJSON(data []byte) error {
	parsed, err := decodeClosedEnum(data, "capture mode", []string{"window"})
	*value = CaptureMode(parsed)
	return err
}

func (value *InputRoute) UnmarshalJSON(data []byte) error {
	parsed, err := decodeClosedEnum(data, "input route", []string{"semantic", "foreground"})
	*value = InputRoute(parsed)
	return err
}

func (value *ActionKind) UnmarshalJSON(data []byte) error {
	parsed, err := decodeClosedEnum(data, "action kind", actionKinds)
	*value = ActionKind(parsed)
	return err
}

func (value *PermissionState) UnmarshalJSON(data []byte) error {
	parsed, err := decodeClosedEnum(data, "permission state", []string{"granted", "denied", "not_determined", "not_applicable", "unknown"})
	*value = PermissionState(parsed)
	return err
}

func (value *PermissionMode) UnmarshalJSON(data []byte) error {
	parsed, err := decodeClosedEnum(data, "permission mode", permissionModes)
	*value = PermissionMode(parsed)
	return err
}

func (value *AccessibilityMode) UnmarshalJSON(data []byte) error {
	parsed, err := decodeClosedEnum(data, "accessibility mode", accessibilityModes)
	*value = AccessibilityMode(parsed)
	return err
}

func (value *PointerButton) UnmarshalJSON(data []byte) error {
	parsed, err := decodeClosedEnum(data, "pointer button", []string{"left", "middle", "right"})
	*value = PointerButton(parsed)
	return err
}

func (value *DeliveryMode) UnmarshalJSON(data []byte) error {
	parsed, err := decodeClosedEnum(data, "delivery mode", []string{"semantic", "foreground"})
	*value = DeliveryMode(parsed)
	return err
}

func (value *SignatureStatus) UnmarshalJSON(data []byte) error {
	parsed, err := decodeClosedEnum(data, "signature status", []string{"verified", "invalid", "unsigned", "unknown"})
	*value = SignatureStatus(parsed)
	return err
}

func (value *MutationStatus) UnmarshalJSON(data []byte) error {
	parsed, err := decodeClosedEnum(data, "mutation status", mutationStatuses)
	*value = MutationStatus(parsed)
	return err
}

func (value *ErrorCode) UnmarshalJSON(data []byte) error {
	parsed, err := decodeClosedEnum(data, "public error code", errorCodes)
	*value = ErrorCode(parsed)
	return err
}

func (value *TruncationReason) UnmarshalJSON(data []byte) error {
	parsed, err := decodeClosedEnum(data, "truncation reason", []string{"node_limit", "depth_limit", "byte_limit", "deadline", "provider_failure"})
	*value = TruncationReason(parsed)
	return err
}

func (value *ApplicationProvenance) UnmarshalJSON(data []byte) error {
	type provenance ApplicationProvenance
	var decoded provenance
	if err := decodeStrict(data, &decoded); err != nil {
		return err
	}
	switch decoded.Platform {
	case PlatformMacOS:
		if decoded.Publisher != nil || decoded.SignatureStatus != nil {
			return fmt.Errorf("nexus-cua: windows provenance fields on macOS descriptor")
		}
	case PlatformWindows:
		if decoded.ExecutablePath == nil || decoded.SignatureStatus == nil {
			return fmt.Errorf("nexus-cua: incomplete Windows provenance")
		}
		if decoded.BundleID != nil || decoded.SigningTeamID != nil || decoded.DesignatedRequirement != nil {
			return fmt.Errorf("nexus-cua: macOS provenance fields on Windows descriptor")
		}
	case PlatformUnsupported:
		if decoded.BundleID != nil || decoded.SigningTeamID != nil || decoded.DesignatedRequirement != nil || decoded.Publisher != nil || decoded.SignatureStatus != nil {
			return fmt.Errorf("nexus-cua: native provenance fields on unsupported descriptor")
		}
	}
	*value = ApplicationProvenance(decoded)
	return nil
}
