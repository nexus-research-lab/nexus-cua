package nexuscua

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"runtime"
	"strings"
	"sync"
	"time"
)

const (
	defaultMaxFrameBytes       = 1024 * 1024
	defaultReconcileHorizon    = 10 * time.Minute
	minimumTransportTokenBytes = 32
	maximumTransportTokenBytes = 4 * 1024
)

// Config identifies one explicitly supervised local sidecar. The token is read
// from a host-private file and is never exposed by Client.
type Config struct {
	Endpoint              string
	TokenFile             string
	MaxFrameBytes         uint32
	ReconciliationHorizon time.Duration
}

type Client struct {
	transport             roundTripper
	token                 string
	maxFrameBytes         uint32
	reconciliationHorizon time.Duration
	now                   func() time.Time
	newRequestID          func() (RequestID, error)
}

func (*Client) String() string   { return "nexuscua.Client([PRIVATE TRANSPORT])" }
func (*Client) GoString() string { return "nexuscua.Client([PRIVATE TRANSPORT])" }

func NewClient(config Config) (*Client, error) {
	if config.Endpoint == "" {
		return nil, errors.New("nexus-cua: endpoint is required")
	}
	if config.TokenFile == "" {
		return nil, errors.New("nexus-cua: token file is required")
	}
	token, err := readPrivateToken(config.TokenFile)
	if err != nil {
		return nil, err
	}
	maxFrameBytes := config.MaxFrameBytes
	if maxFrameBytes == 0 {
		maxFrameBytes = defaultMaxFrameBytes
	}
	horizon := config.ReconciliationHorizon
	if horizon == 0 {
		horizon = defaultReconcileHorizon
	}
	if horizon < defaultReconcileHorizon {
		return nil, fmt.Errorf("nexus-cua: reconciliation horizon must be at least %s", defaultReconcileHorizon)
	}
	return &Client{
		transport:             nativeTransport{endpoint: config.Endpoint},
		token:                 token,
		maxFrameBytes:         maxFrameBytes,
		reconciliationHorizon: horizon,
		now:                   time.Now,
		newRequestID:          randomRequestID,
	}, nil
}

func (client *Client) GetCapabilities(ctx context.Context, timeout time.Duration) (DriverCapabilities, error) {
	return requestResult[DriverCapabilities](client, ctx, timeout, commandEnvelope{Operation: "get_capabilities"}, "capabilities")
}

func (client *Client) GetPermissionStatus(ctx context.Context, timeout time.Duration) (PermissionStatus, error) {
	return requestResult[PermissionStatus](client, ctx, timeout, commandEnvelope{Operation: "get_permission_status"}, "permission_status")
}

func (client *Client) DiscoverApplications(ctx context.Context, timeout time.Duration) (DiscoverApplicationsOutput, error) {
	return requestResult[DiscoverApplicationsOutput](client, ctx, timeout, commandEnvelope{Operation: "discover_applications"}, "applications_discovered")
}

func (client *Client) DiscoverLaunchableApplications(ctx context.Context, timeout time.Duration) (DiscoverLaunchableApplicationsOutput, error) {
	return requestResult[DiscoverLaunchableApplicationsOutput](client, ctx, timeout, commandEnvelope{Operation: "discover_launchable_applications"}, "launchable_applications_discovered")
}

// SelectApplication resolves a trusted-host selector without silently choosing
// an arbitrary process. Exact display-name or stable application-ID matches
// take precedence; a substring is accepted only when it identifies one app.
func SelectApplication(applications []DiscoveredApplication, selector string) (*DiscoveredApplication, error) {
	if strings.TrimSpace(selector) == "" {
		return nil, errors.New("nexus-cua: application selector is required")
	}
	exact := make([]int, 0, 1)
	for index := range applications {
		if applications[index].Name == selector || applications[index].ApplicationID == selector {
			exact = append(exact, index)
		}
	}
	if len(exact) == 1 {
		return &applications[exact[0]], nil
	}
	if len(exact) > 1 {
		return nil, fmt.Errorf("nexus-cua: application selector %q has %d exact matches", selector, len(exact))
	}
	partial := make([]int, 0, 1)
	for index := range applications {
		if strings.Contains(applications[index].Name, selector) || strings.Contains(applications[index].ApplicationID, selector) {
			partial = append(partial, index)
		}
	}
	if len(partial) == 1 {
		return &applications[partial[0]], nil
	}
	if len(partial) == 0 {
		return nil, fmt.Errorf("nexus-cua: application selector %q did not match discovery", selector)
	}
	return nil, fmt.Errorf("nexus-cua: application selector %q is ambiguous across %d matches; use an exact name or stable application ID", selector, len(partial))
}

// SelectWindow resolves a trusted-host title selector without relying on the
// platform's window ordering. Exact titles take precedence; a substring must
// identify exactly one top-level window.
func SelectWindow(windows []WindowSummary, selector string) (*WindowSummary, error) {
	if strings.TrimSpace(selector) == "" {
		return nil, errors.New("nexus-cua: window selector is required")
	}
	exact := make([]int, 0, 1)
	for index := range windows {
		if windows[index].Title == selector {
			exact = append(exact, index)
		}
	}
	if len(exact) == 1 {
		return &windows[exact[0]], nil
	}
	if len(exact) > 1 {
		return nil, fmt.Errorf("nexus-cua: window selector %q has %d exact matches", selector, len(exact))
	}
	partial := make([]int, 0, 1)
	for index := range windows {
		if strings.Contains(windows[index].Title, selector) {
			partial = append(partial, index)
		}
	}
	if len(partial) == 1 {
		return &windows[partial[0]], nil
	}
	if len(partial) == 0 {
		return nil, fmt.Errorf("nexus-cua: window selector %q did not match the selected application", selector)
	}
	return nil, fmt.Errorf("nexus-cua: window selector %q is ambiguous across %d matches; use an exact title", selector, len(partial))
}

func (client *Client) OpenSession(ctx context.Context, input OpenSessionInput, timeout time.Duration) (OpenSessionOutput, error) {
	if err := validateManifest(input.Manifest); err != nil {
		return OpenSessionOutput{}, err
	}
	return requestResult[OpenSessionOutput](client, ctx, timeout, commandEnvelope{Operation: "open_session", Input: input}, "session_opened")
}

func (client *Client) CloseSession(ctx context.Context, sessionID SessionID, timeout time.Duration) error {
	return requestAcknowledged(client, ctx, timeout, commandEnvelope{
		Operation: "close_session",
		Input: struct {
			SessionID SessionID `json:"session_id"`
		}{SessionID: sessionID},
	})
}

func (client *Client) ListApps(ctx context.Context, sessionID SessionID, timeout time.Duration) ([]ApplicationSummary, error) {
	return requestResult[[]ApplicationSummary](client, ctx, timeout, commandEnvelope{
		Operation: "list_apps",
		Input: struct {
			SessionID SessionID `json:"session_id"`
		}{SessionID: sessionID},
	}, "apps")
}

func (client *Client) ListWindows(ctx context.Context, sessionID SessionID, appRef *AppRef, timeout time.Duration) ([]WindowSummary, error) {
	input := struct {
		SessionID SessionID `json:"session_id"`
		AppRef    *AppRef   `json:"app_ref"`
	}{SessionID: sessionID, AppRef: appRef}
	return requestResult[[]WindowSummary](client, ctx, timeout, commandEnvelope{Operation: "list_windows", Input: input}, "windows")
}

func (client *Client) ListDisplays(ctx context.Context, sessionID SessionID, timeout time.Duration) ([]DisplaySummary, error) {
	return requestResult[[]DisplaySummary](client, ctx, timeout, commandEnvelope{
		Operation: "list_displays",
		Input: struct {
			SessionID SessionID `json:"session_id"`
		}{SessionID: sessionID},
	}, "displays")
}

func (client *Client) ObserveDisplay(ctx context.Context, input ObserveDisplayInput, timeout time.Duration) (DisplayObservation, error) {
	return requestResult[DisplayObservation](client, ctx, timeout, commandEnvelope{Operation: "observe_display", Input: input}, "display_observed")
}

func (client *Client) ObserveWindow(ctx context.Context, input ObserveWindowInput, timeout time.Duration) (WindowObservation, error) {
	if !contains(accessibilityModes, string(input.Accessibility)) {
		return WindowObservation{}, fmt.Errorf("nexus-cua: invalid accessibility mode %q", input.Accessibility)
	}
	return requestResult[WindowObservation](client, ctx, timeout, commandEnvelope{Operation: "observe_window", Input: input}, "window_observed")
}

func (client *Client) ReadClipboard(ctx context.Context, sessionID SessionID, timeout time.Duration) (ClipboardSnapshot, error) {
	return requestResult[ClipboardSnapshot](client, ctx, timeout, commandEnvelope{
		Operation: "read_clipboard",
		Input: struct {
			SessionID SessionID `json:"session_id"`
		}{SessionID: sessionID},
	}, "clipboard_read")
}

func (client *Client) VerifyState(ctx context.Context, input VerifyStateInput, timeout time.Duration) (VerificationOutput, error) {
	predicate, err := marshalPredicate(input.Predicate)
	if err != nil {
		return VerificationOutput{}, err
	}
	wireInput := struct {
		SessionID SessionID      `json:"session_id"`
		WindowRef WindowRef      `json:"window_ref"`
		Predicate map[string]any `json:"predicate"`
	}{input.SessionID, input.WindowRef, predicate}
	return requestResult[VerificationOutput](client, ctx, timeout, commandEnvelope{Operation: "verify_state", Input: wireInput}, "state_verified")
}

// ReconcileMutation queries a retained mutation identity without dispatching
// the original command again. wait may only observe already-admitted work.
func (client *Client) ReconcileMutation(ctx context.Context, requestID RequestID, wait, timeout time.Duration) (MutationReconciliation, error) {
	if !validRequestID(requestID) {
		return MutationReconciliation{}, errors.New("nexus-cua: request ID must be normalized ASCII within 128 bytes")
	}
	if wait < 0 || wait > 120*time.Second {
		return MutationReconciliation{}, errors.New("nexus-cua: reconciliation wait must be between zero and 120 seconds")
	}
	waitMS := uint32(wait / time.Millisecond)
	input := struct {
		RequestID RequestID `json:"request_id"`
		WaitMS    uint32    `json:"wait_ms"`
	}{RequestID: requestID, WaitMS: waitMS}
	return requestResult[MutationReconciliation](client, ctx, timeout, commandEnvelope{
		Operation: "reconcile_mutation", Input: input,
	}, "mutation_reconciled")
}

// ActionRequest owns the one request identity that may be reconciled after an
// indeterminate wait. It contains no transport token and cannot be retargeted.
type ActionRequest struct {
	mu          sync.Mutex
	requestID   RequestID
	command     commandEnvelope
	createdAt   time.Time
	lastWait    time.Duration
	completed   bool
	invalidated bool
}

// ClipboardRequest owns the one request identity that may be reconciled after
// an indeterminate guarded clipboard replacement.
type ClipboardRequest struct {
	mu          sync.Mutex
	requestID   RequestID
	command     commandEnvelope
	createdAt   time.Time
	lastWait    time.Duration
	completed   bool
	invalidated bool
}

func (request *ClipboardRequest) RequestID() RequestID { return request.requestID }
func (request *ClipboardRequest) String() string {
	return fmt.Sprintf("nexuscua.ClipboardRequest(%s)", request.requestID)
}
func (request *ClipboardRequest) GoString() string { return request.String() }

// ClipboardIndeterminateError means the exact ClipboardRequest must be
// reconciled and the replacement must not be submitted under a new identity.
type ClipboardIndeterminateError struct {
	Request *ClipboardRequest
	Cause   error
}

func (error *ClipboardIndeterminateError) Error() string {
	return fmt.Sprintf("nexus-cua: clipboard mutation %s is indeterminate: %v", error.Request.requestID, error.Cause)
}

func (error *ClipboardIndeterminateError) Unwrap() error { return error.Cause }

func (request *ActionRequest) RequestID() RequestID { return request.requestID }
func (request *ActionRequest) String() string {
	return fmt.Sprintf("nexuscua.ActionRequest(%s)", request.requestID)
}
func (request *ActionRequest) GoString() string { return request.String() }

// MutationIndeterminateError means the caller must reconcile the attached
// ActionRequest with the same Client and must not construct a fresh mutation.
type MutationIndeterminateError struct {
	Request *ActionRequest
	Cause   error
}

func (error *MutationIndeterminateError) Error() string {
	return fmt.Sprintf("nexus-cua: mutation %s is indeterminate: %v", error.Request.requestID, error.Cause)
}

func (error *MutationIndeterminateError) Unwrap() error { return error.Cause }

// LaunchRequest owns the one request identity used to reconcile a cold launch.
type LaunchRequest struct {
	mu          sync.Mutex
	requestID   RequestID
	command     commandEnvelope
	createdAt   time.Time
	lastWait    time.Duration
	completed   bool
	invalidated bool
}

func (request *LaunchRequest) RequestID() RequestID { return request.requestID }
func (request *LaunchRequest) String() string {
	return fmt.Sprintf("nexuscua.LaunchRequest(%s)", request.requestID)
}
func (request *LaunchRequest) GoString() string { return request.String() }

// LaunchIndeterminateError means the exact LaunchRequest must be reconciled
// and the launch must never be submitted under a new request identity.
type LaunchIndeterminateError struct {
	Request *LaunchRequest
	Cause   error
}

func (error *LaunchIndeterminateError) Error() string {
	return fmt.Sprintf("nexus-cua: launch %s is indeterminate: %v", error.Request.requestID, error.Cause)
}

func (error *LaunchIndeterminateError) Unwrap() error { return error.Cause }

// LaunchApplication consumes one fresh launch reference and performs the first
// bounded wait for the newly started process generation.
func (client *Client) LaunchApplication(ctx context.Context, input LaunchApplicationInput, timeout time.Duration) (ApplicationLaunchOutput, *LaunchRequest, error) {
	if strings.TrimSpace(string(input.LaunchRef)) == "" {
		return ApplicationLaunchOutput{}, nil, errors.New("nexus-cua: launch reference is required")
	}
	requestID, err := client.newRequestID()
	if err != nil {
		return ApplicationLaunchOutput{}, nil, err
	}
	request := &LaunchRequest{
		requestID: requestID,
		command: commandEnvelope{
			Operation: "launch_application",
			Input:     input,
		},
		createdAt: client.now(),
	}
	result, err := client.waitForLaunch(ctx, request, timeout)
	return result, request, err
}

// ReconcileLaunch extends the wait for the exact prior launch mutation.
func (client *Client) ReconcileLaunch(ctx context.Context, request *LaunchRequest, timeout time.Duration) (ApplicationLaunchOutput, error) {
	if request == nil {
		return ApplicationLaunchOutput{}, errors.New("nexus-cua: launch request is required")
	}
	return client.waitForLaunch(ctx, request, timeout)
}

func (client *Client) waitForLaunch(ctx context.Context, request *LaunchRequest, timeout time.Duration) (ApplicationLaunchOutput, error) {
	request.mu.Lock()
	defer request.mu.Unlock()
	if request.completed {
		return ApplicationLaunchOutput{}, errors.New("nexus-cua: mutation request is already complete")
	}
	if request.invalidated || client.now().Sub(request.createdAt) > client.reconciliationHorizon {
		request.invalidated = true
		request.command = commandEnvelope{}
		return ApplicationLaunchOutput{}, &LaunchIndeterminateError{Request: request, Cause: errors.New("reconciliation horizon elapsed")}
	}
	if request.lastWait != 0 && timeout < request.lastWait {
		return ApplicationLaunchOutput{}, errors.New("nexus-cua: reconciliation may only extend the prior wait deadline")
	}
	timeoutMS, err := timeoutMilliseconds(timeout)
	if err != nil {
		return ApplicationLaunchOutput{}, err
	}
	request.lastWait = timeout
	response, err := client.roundTrip(ctx, request.requestID, timeoutMS, request.command)
	if err != nil {
		return ApplicationLaunchOutput{}, &LaunchIndeterminateError{Request: request, Cause: err}
	}
	result, err := decodeOutcome[ApplicationLaunchOutput](response.Outcome, "application_launched")
	if err == nil {
		request.completed = true
		request.command = commandEnvelope{}
		return result, nil
	}
	var publicError *CUAError
	if errors.As(err, &publicError) && publicError.MutationStatus == MutationNotDispatched {
		request.completed = true
		request.command = commandEnvelope{}
		return ApplicationLaunchOutput{}, err
	}
	return ApplicationLaunchOutput{}, &LaunchIndeterminateError{Request: request, Cause: err}
}

// PerformAction creates one immutable mutation request and performs its first
// wait. The returned request must be retained when the error is indeterminate.
func (client *Client) PerformAction(ctx context.Context, input PerformActionInput, timeout time.Duration) (ActionOutput, *ActionRequest, error) {
	action, err := marshalAction(input.Action)
	if err != nil {
		return ActionOutput{}, nil, err
	}
	requestID, err := client.newRequestID()
	if err != nil {
		return ActionOutput{}, nil, err
	}
	wireInput := struct {
		SessionID     SessionID      `json:"session_id"`
		WindowRef     WindowRef      `json:"window_ref"`
		ObservationID ObservationID  `json:"observation_id"`
		Action        map[string]any `json:"action"`
	}{input.SessionID, input.WindowRef, input.ObservationID, action}
	request := &ActionRequest{
		requestID: requestID,
		command:   commandEnvelope{Operation: "perform_action", Input: wireInput},
		createdAt: client.now(),
	}
	result, err := client.waitForAction(ctx, request, timeout)
	return result, request, err
}

// ReconcileAction extends the wait for the exact prior mutation. A shorter
// wait, an expired reconciliation horizon, or a completed mutation is rejected
// locally without sending anything.
func (client *Client) ReconcileAction(ctx context.Context, request *ActionRequest, timeout time.Duration) (ActionOutput, error) {
	if request == nil {
		return ActionOutput{}, errors.New("nexus-cua: action request is required")
	}
	return client.waitForAction(ctx, request, timeout)
}

// WriteClipboard consumes one fresh clipboard guard and performs the first
// bounded wait for the replacement result.
func (client *Client) WriteClipboard(ctx context.Context, input ClipboardWriteInput, timeout time.Duration) (ClipboardWriteOutput, *ClipboardRequest, error) {
	if strings.TrimSpace(string(input.ClipboardRef)) == "" {
		return ClipboardWriteOutput{}, nil, errors.New("nexus-cua: clipboard reference is required")
	}
	requestID, err := client.newRequestID()
	if err != nil {
		return ClipboardWriteOutput{}, nil, err
	}
	wireInput := struct {
		SessionID    SessionID     `json:"session_id"`
		ClipboardRef ClipboardRef  `json:"clipboard_ref"`
		Text         SensitiveText `json:"text"`
	}{input.SessionID, input.ClipboardRef, input.Text}
	request := &ClipboardRequest{
		requestID: requestID,
		command:   commandEnvelope{Operation: "write_clipboard", Input: wireInput},
		createdAt: client.now(),
	}
	result, err := client.waitForClipboard(ctx, request, timeout)
	return result, request, err
}

// ReconcileClipboard extends the wait for the exact prior clipboard mutation.
func (client *Client) ReconcileClipboard(ctx context.Context, request *ClipboardRequest, timeout time.Duration) (ClipboardWriteOutput, error) {
	if request == nil {
		return ClipboardWriteOutput{}, errors.New("nexus-cua: clipboard request is required")
	}
	return client.waitForClipboard(ctx, request, timeout)
}

func (client *Client) waitForClipboard(ctx context.Context, request *ClipboardRequest, timeout time.Duration) (ClipboardWriteOutput, error) {
	request.mu.Lock()
	defer request.mu.Unlock()
	if request.completed {
		return ClipboardWriteOutput{}, errors.New("nexus-cua: clipboard mutation request is already complete")
	}
	if request.invalidated || client.now().Sub(request.createdAt) > client.reconciliationHorizon {
		request.invalidated = true
		request.command = commandEnvelope{}
		return ClipboardWriteOutput{}, &ClipboardIndeterminateError{Request: request, Cause: errors.New("reconciliation horizon elapsed")}
	}
	if request.lastWait != 0 && timeout < request.lastWait {
		return ClipboardWriteOutput{}, errors.New("nexus-cua: reconciliation may only extend the prior wait deadline")
	}
	timeoutMS, err := timeoutMilliseconds(timeout)
	if err != nil {
		return ClipboardWriteOutput{}, err
	}
	request.lastWait = timeout
	response, err := client.roundTrip(ctx, request.requestID, timeoutMS, request.command)
	if err != nil {
		return ClipboardWriteOutput{}, &ClipboardIndeterminateError{Request: request, Cause: err}
	}
	result, err := decodeOutcome[ClipboardWriteOutput](response.Outcome, "clipboard_written")
	if err == nil {
		request.completed = true
		request.command = commandEnvelope{}
		return result, nil
	}
	var publicError *CUAError
	if errors.As(err, &publicError) && publicError.MutationStatus == MutationNotDispatched {
		request.completed = true
		request.command = commandEnvelope{}
		return ClipboardWriteOutput{}, err
	}
	return ClipboardWriteOutput{}, &ClipboardIndeterminateError{Request: request, Cause: err}
}

func (client *Client) waitForAction(ctx context.Context, request *ActionRequest, timeout time.Duration) (ActionOutput, error) {
	request.mu.Lock()
	defer request.mu.Unlock()
	if request.completed {
		return ActionOutput{}, errors.New("nexus-cua: mutation request is already complete")
	}
	if request.invalidated || client.now().Sub(request.createdAt) > client.reconciliationHorizon {
		request.invalidated = true
		request.command = commandEnvelope{}
		return ActionOutput{}, &MutationIndeterminateError{Request: request, Cause: errors.New("reconciliation horizon elapsed")}
	}
	if request.lastWait != 0 && timeout < request.lastWait {
		return ActionOutput{}, errors.New("nexus-cua: reconciliation may only extend the prior wait deadline")
	}
	timeoutMS, err := timeoutMilliseconds(timeout)
	if err != nil {
		return ActionOutput{}, err
	}
	request.lastWait = timeout
	response, err := client.roundTrip(ctx, request.requestID, timeoutMS, request.command)
	if err != nil {
		return ActionOutput{}, &MutationIndeterminateError{Request: request, Cause: err}
	}
	result, err := decodeOutcome[ActionOutput](response.Outcome, "action_performed")
	if err == nil {
		request.completed = true
		request.command = commandEnvelope{}
		return result, nil
	}
	var publicError *CUAError
	if errors.As(err, &publicError) {
		if publicError.MutationStatus == MutationNotDispatched {
			request.completed = true
			request.command = commandEnvelope{}
			return ActionOutput{}, err
		}
		return ActionOutput{}, &MutationIndeterminateError{Request: request, Cause: err}
	}
	return ActionOutput{}, &MutationIndeterminateError{Request: request, Cause: err}
}

func requestResult[T any](client *Client, ctx context.Context, timeout time.Duration, command commandEnvelope, resultType string) (T, error) {
	var zero T
	requestID, err := client.newRequestID()
	if err != nil {
		return zero, err
	}
	timeoutMS, err := timeoutMilliseconds(timeout)
	if err != nil {
		return zero, err
	}
	response, err := client.roundTrip(ctx, requestID, timeoutMS, command)
	if err != nil {
		return zero, err
	}
	return decodeOutcome[T](response.Outcome, resultType)
}

func requestAcknowledged(client *Client, ctx context.Context, timeout time.Duration, command commandEnvelope) error {
	requestID, err := client.newRequestID()
	if err != nil {
		return err
	}
	timeoutMS, err := timeoutMilliseconds(timeout)
	if err != nil {
		return err
	}
	response, err := client.roundTrip(ctx, requestID, timeoutMS, command)
	if err != nil {
		return err
	}
	return decodeAcknowledged(response.Outcome)
}

func (client *Client) roundTrip(ctx context.Context, requestID RequestID, timeoutMS uint32, command commandEnvelope) (responseEnvelope, error) {
	if err := ctx.Err(); err != nil {
		return responseEnvelope{}, err
	}
	request := requestEnvelope{
		ProtocolVersion: ProtocolVersion,
		RequestID:       requestID,
		TimeoutMS:       timeoutMS,
		Authorization:   client.token,
		Command:         command,
	}
	payload, err := json.Marshal(request)
	if err != nil {
		return responseEnvelope{}, err
	}
	waitContext, cancel := context.WithTimeout(ctx, time.Duration(timeoutMS)*time.Millisecond)
	defer cancel()
	response, err := client.transport.RoundTrip(waitContext, payload, client.maxFrameBytes)
	if err != nil {
		return responseEnvelope{}, err
	}
	return validateResponseEnvelope(response, requestID)
}

func validateManifest(manifest CapabilityManifest) error {
	if !contains(permissionModes, string(manifest.Mode)) {
		return fmt.Errorf("nexus-cua: invalid permission mode %q", manifest.Mode)
	}
	if len(manifest.ApplicationRefs) == 0 && !manifest.AllowDisplayCapture && !manifest.AllowClipboardRead && !manifest.AllowClipboardWrite {
		return errors.New("nexus-cua: at least one discovery reference, display-capture authority, or clipboard authority is required")
	}
	if manifest.TTLSeconds == 0 {
		return errors.New("nexus-cua: session TTL must be finite and non-zero")
	}
	for _, action := range manifest.AllowedActions {
		if !contains(actionKinds, string(action)) {
			return fmt.Errorf("nexus-cua: invalid action kind %q", action)
		}
	}
	if manifest.Mode == PermissionReadOnly && (len(manifest.AllowedActions) != 0 || manifest.AllowForegroundInput || manifest.RestoreForegroundAfterInput || manifest.AllowClipboardWrite) {
		return errors.New("nexus-cua: read-only sessions cannot grant mutation authority")
	}
	if manifest.AllowClipboardWrite && !manifest.AllowClipboardRead {
		return errors.New("nexus-cua: clipboard writes require clipboard-read authority")
	}
	if manifest.RestoreForegroundAfterInput && !manifest.AllowForegroundInput {
		return errors.New("nexus-cua: foreground restoration requires foreground input authority")
	}
	return nil
}

func timeoutMilliseconds(timeout time.Duration) (uint32, error) {
	if timeout <= 0 {
		return 0, errors.New("nexus-cua: timeout must be positive")
	}
	milliseconds := timeout.Milliseconds()
	if milliseconds <= 0 || milliseconds > int64(^uint32(0)) {
		return 0, errors.New("nexus-cua: timeout is outside the wire bound")
	}
	return uint32(milliseconds), nil
}

func validRequestID(requestID RequestID) bool {
	value := string(requestID)
	if value == "" || len(value) > 128 {
		return false
	}
	for _, character := range []byte(value) {
		if (character < 'a' || character > 'z') &&
			(character < 'A' || character > 'Z') &&
			(character < '0' || character > '9') &&
			character != '-' && character != '_' && character != '.' && character != ':' {
			return false
		}
	}
	return true
}

func randomRequestID() (RequestID, error) {
	var random [16]byte
	if _, err := rand.Read(random[:]); err != nil {
		return "", fmt.Errorf("nexus-cua: generate request ID: %w", err)
	}
	return RequestID("request_" + hex.EncodeToString(random[:])), nil
}

func readPrivateToken(path string) (string, error) {
	info, err := os.Stat(path)
	if err != nil {
		return "", fmt.Errorf("nexus-cua: inspect token file: %w", err)
	}
	if !info.Mode().IsRegular() {
		return "", errors.New("nexus-cua: token path must be a regular file")
	}
	if runtime.GOOS != "windows" && info.Mode().Perm()&0o077 != 0 {
		return "", errors.New("nexus-cua: token file must not be accessible by group or other users")
	}
	contents, err := os.ReadFile(path)
	if err != nil {
		return "", fmt.Errorf("nexus-cua: read token file: %w", err)
	}
	value := strings.TrimSpace(string(contents))
	if len(value) < minimumTransportTokenBytes || len(value) > maximumTransportTokenBytes || strings.IndexFunc(value, func(r rune) bool { return r == ' ' || r == '\t' || r == '\r' || r == '\n' }) >= 0 {
		return "", errors.New("nexus-cua: token file must contain 32 to 4096 non-whitespace bytes")
	}
	return value, nil
}

var permissionModes = []string{"read_only", "bounded"}
var accessibilityModes = []string{"disabled", "interactive", "full"}
var actionKinds = []string{
	"focus_window", "set_window_bounds", "set_window_minimized", "terminate_application", "focus_element", "invoke_element", "click_point", "press_point", "set_value",
	"set_range_value", "toggle_element", "select_element", "set_expanded", "move_pointer", "type_text",
	"press_keys", "scroll", "drag",
}
