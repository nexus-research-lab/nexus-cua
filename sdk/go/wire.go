package nexuscua

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
)

type requestEnvelope struct {
	ProtocolVersion string          `json:"protocol_version"`
	RequestID       RequestID       `json:"request_id"`
	TimeoutMS       uint32          `json:"timeout_ms"`
	Authorization   string          `json:"authorization"`
	Command         commandEnvelope `json:"command"`
}

type commandEnvelope struct {
	Operation string `json:"operation"`
	Input     any    `json:"input,omitempty"`
}

type responseEnvelope struct {
	ProtocolVersion string          `json:"protocol_version"`
	RequestID       RequestID       `json:"request_id"`
	Outcome         json.RawMessage `json:"outcome"`
}

type outcomeTag struct {
	Status string `json:"status"`
}

type successOutcome struct {
	Status string          `json:"status"`
	Result json.RawMessage `json:"result"`
}

type errorOutcome struct {
	Status string   `json:"status"`
	Error  CUAError `json:"error"`
}

type resultTag struct {
	ResultType string `json:"result_type"`
}

type resultWithData[T any] struct {
	ResultType string `json:"result_type"`
	Data       T      `json:"data"`
}

type acknowledgedResult struct {
	ResultType string `json:"result_type"`
}

func decodeStrict(data []byte, value any) error {
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(value); err != nil {
		return err
	}
	if err := decoder.Decode(&struct{}{}); !errorsIsEOF(err) {
		return fmt.Errorf("nexus-cua: trailing JSON value")
	}
	return nil
}

func errorsIsEOF(err error) bool { return err == io.EOF }

func decodeOutcome[T any](data []byte, expected string) (T, error) {
	var zero T
	var tag outcomeTag
	if err := decodeStrictTag(data, &tag); err != nil {
		return zero, fmt.Errorf("decode outcome tag: %w", err)
	}
	switch tag.Status {
	case "error":
		var outcome errorOutcome
		if err := decodeStrict(data, &outcome); err != nil {
			return zero, fmt.Errorf("decode error outcome: %w", err)
		}
		if err := validateCUAError(&outcome.Error); err != nil {
			return zero, err
		}
		return zero, &outcome.Error
	case "success":
		var outcome successOutcome
		if err := decodeStrict(data, &outcome); err != nil {
			return zero, fmt.Errorf("decode success outcome: %w", err)
		}
		var result resultWithData[T]
		if err := decodeStrict(outcome.Result, &result); err != nil {
			return zero, fmt.Errorf("decode %s result: %w", expected, err)
		}
		if result.ResultType != expected {
			return zero, fmt.Errorf("nexus-cua: expected result %q, received %q", expected, result.ResultType)
		}
		return result.Data, nil
	default:
		return zero, fmt.Errorf("nexus-cua: unknown outcome status %q", tag.Status)
	}
}

func decodeAcknowledged(data []byte) error {
	var tag outcomeTag
	if err := decodeStrictTag(data, &tag); err != nil {
		return err
	}
	if tag.Status == "error" {
		var outcome errorOutcome
		if err := decodeStrict(data, &outcome); err != nil {
			return err
		}
		if err := validateCUAError(&outcome.Error); err != nil {
			return err
		}
		return &outcome.Error
	}
	if tag.Status != "success" {
		return fmt.Errorf("nexus-cua: unknown outcome status %q", tag.Status)
	}
	var outcome successOutcome
	if err := decodeStrict(data, &outcome); err != nil {
		return err
	}
	var result acknowledgedResult
	if err := decodeStrict(outcome.Result, &result); err != nil {
		return err
	}
	if result.ResultType != "acknowledged" {
		return fmt.Errorf("nexus-cua: expected acknowledged result, received %q", result.ResultType)
	}
	return nil
}

func decodeStrictTag(data []byte, value any) error {
	if err := json.Unmarshal(data, value); err != nil {
		return err
	}
	return nil
}

func validateCUAError(value *CUAError) error {
	if !contains(errorCodes, string(value.Code)) {
		return fmt.Errorf("nexus-cua: unknown public error code %q", value.Code)
	}
	if !contains(mutationStatuses, string(value.MutationStatus)) {
		return fmt.Errorf("nexus-cua: unknown mutation status %q", value.MutationStatus)
	}
	return nil
}

func validateResponseEnvelope(payload []byte, requestID RequestID) (responseEnvelope, error) {
	var response responseEnvelope
	if err := decodeStrict(payload, &response); err != nil {
		return response, fmt.Errorf("decode response envelope: %w", err)
	}
	if response.ProtocolVersion != ProtocolVersion {
		return response, fmt.Errorf("nexus-cua: unsupported response protocol %q", response.ProtocolVersion)
	}
	if response.RequestID != requestID {
		return response, fmt.Errorf("nexus-cua: response request_id mismatch")
	}
	return response, nil
}

func marshalAction(action Action) (map[string]any, error) {
	var kind string
	switch value := action.(type) {
	case FocusWindow:
		kind = string(ActionFocusWindow)
	case FocusElement:
		kind = string(ActionFocusElement)
	case InvokeElement:
		kind = string(ActionInvokeElement)
	case ClickPoint:
		if !contains([]string{"left", "middle", "right"}, string(value.Button)) || value.Count == 0 {
			return nil, fmt.Errorf("nexus-cua: invalid click button or count")
		}
		kind = string(ActionClickPoint)
	case SetValue:
		kind = string(ActionSetValue)
	case ToggleElement:
		kind = string(ActionToggleElement)
	case SelectElement:
		kind = string(ActionSelectElement)
	case SetExpanded:
		kind = string(ActionSetExpanded)
	case MovePointer:
		kind = string(ActionMovePointer)
	case TypeText:
		kind = string(ActionTypeText)
	case PressKeys:
		if len(value.Keys) == 0 {
			return nil, fmt.Errorf("nexus-cua: key chord must not be empty")
		}
		for _, key := range value.Keys {
			if key == "" {
				return nil, fmt.Errorf("nexus-cua: key chord contains an empty key")
			}
		}
		kind = string(ActionPressKeys)
	case Scroll:
		kind = string(ActionScroll)
	case Drag:
		kind = string(ActionDrag)
	default:
		return nil, fmt.Errorf("nexus-cua: unsupported action type %T", action)
	}
	return taggedValue(kind, action)
}

func marshalPredicate(predicate StatePredicate) (map[string]any, error) {
	var kind string
	switch predicate.(type) {
	case WindowTitleContains:
		kind = "window_title_contains"
	case ElementExists:
		kind = "element_exists"
	case BoundsContained:
		kind = "bounds_contained"
	default:
		return nil, fmt.Errorf("nexus-cua: unsupported predicate type %T", predicate)
	}
	return taggedValue(kind, predicate)
}

func taggedValue(kind string, value any) (map[string]any, error) {
	payload, err := json.Marshal(value)
	if err != nil {
		return nil, err
	}
	fields := map[string]any{}
	if err := json.Unmarshal(payload, &fields); err != nil {
		return nil, err
	}
	fields["kind"] = kind
	return fields, nil
}

var errorCodes = []string{
	"protocol_mismatch", "unauthorized", "invalid_request", "busy", "deadline_exceeded",
	"session_unavailable", "stale_discovery", "capability_denied", "reference_not_found",
	"stale_observation", "permission_required", "unsupported", "foreground_required",
	"target_unavailable", "target_unresponsive", "driver_failure", "internal",
}

var mutationStatuses = []string{"not_applicable", "not_dispatched", "indeterminate"}

func contains(values []string, candidate string) bool {
	for _, value := range values {
		if value == candidate {
			return true
		}
	}
	return false
}
