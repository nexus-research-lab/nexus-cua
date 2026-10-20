package nexuscua

import (
	"bytes"
	"context"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"sync"
	"testing"
	"time"
)

type fixtureTransport struct {
	mu       sync.Mutex
	results  map[string]json.RawMessage
	requests [][]byte
}

func (transport *fixtureTransport) RoundTrip(_ context.Context, payload []byte, _ uint32) ([]byte, error) {
	transport.mu.Lock()
	defer transport.mu.Unlock()
	transport.requests = append(transport.requests, append([]byte(nil), payload...))
	var request requestEnvelope
	if err := json.Unmarshal(payload, &request); err != nil {
		return nil, err
	}
	result, ok := transport.results[request.Command.Operation]
	if !ok {
		return nil, fmt.Errorf("missing fixture result for %s", request.Command.Operation)
	}
	return json.Marshal(map[string]any{
		"protocol_version": ProtocolVersion,
		"request_id":       request.RequestID,
		"outcome": map[string]any{
			"status": "success",
			"result": result,
		},
	})
}

func TestCompatibilityFixturesCoverPublicClient(t *testing.T) {
	requests := loadRawArray(t, "requests.json")
	results := loadRawArray(t, "results.json")
	if len(requests) != 17 || len(results) != 17 {
		t.Fatalf("unexpected fixture inventory: requests=%d results=%d", len(requests), len(results))
	}
	resultByOperation := make(map[string]json.RawMessage)
	for index, requestRaw := range requests {
		var request requestEnvelope
		if err := json.Unmarshal(requestRaw, &request); err != nil {
			t.Fatal(err)
		}
		resultByOperation[request.Command.Operation] = results[index]
	}
	transport := &fixtureTransport{results: resultByOperation}
	requestIndex := 0
	client := &Client{
		transport:             transport,
		token:                 "fixture-transport-token",
		maxFrameBytes:         defaultMaxFrameBytes,
		reconciliationHorizon: defaultReconcileHorizon,
		now:                   time.Now,
		newRequestID: func() (RequestID, error) {
			var request requestEnvelope
			if err := json.Unmarshal(requests[requestIndex], &request); err != nil {
				return "", err
			}
			requestIndex++
			return request.RequestID, nil
		},
	}
	ctx := context.Background()
	timeout := 30 * time.Second
	if _, err := client.GetCapabilities(ctx, timeout); err != nil {
		t.Fatal(err)
	}
	if _, err := client.GetPermissionStatus(ctx, timeout); err != nil {
		t.Fatal(err)
	}
	if _, err := client.DiscoverApplications(ctx, timeout); err != nil {
		t.Fatal(err)
	}
	if _, err := client.DiscoverLaunchableApplications(ctx, timeout); err != nil {
		t.Fatal(err)
	}
	if _, _, err := client.LaunchApplication(ctx, LaunchApplicationInput{
		LaunchRef: "launch_fixture", AllowForegroundActivation: true,
	}, timeout); err != nil {
		t.Fatal(err)
	}
	if _, err := client.OpenSession(ctx, OpenSessionInput{Manifest: CapabilityManifest{
		Mode: PermissionBounded, ApplicationRefs: []DiscoveryRef{"discovery_fixture"},
		AllowedActions:       []ActionKind{ActionFocusWindow, ActionInvokeElement, ActionSetRangeValue, ActionClickPoint},
		AllowForegroundInput: true, RestoreForegroundAfterInput: true,
		AllowDisplayCapture: true, AllowClipboardRead: true, AllowClipboardWrite: true,
		TTLSeconds: 300,
	}}, timeout); err != nil {
		t.Fatal(err)
	}
	if err := client.CloseSession(ctx, "session_fixture", timeout); err != nil {
		t.Fatal(err)
	}
	if _, err := client.ListApps(ctx, "session_fixture", timeout); err != nil {
		t.Fatal(err)
	}
	appRef := AppRef("app_fixture")
	if _, err := client.ListWindows(ctx, "session_fixture", &appRef, timeout); err != nil {
		t.Fatal(err)
	}
	if _, err := client.ListDisplays(ctx, "session_fixture", timeout); err != nil {
		t.Fatal(err)
	}
	if _, err := client.ObserveDisplay(ctx, ObserveDisplayInput{
		SessionID: "session_fixture", DisplayRef: "display_fixture",
	}, timeout); err != nil {
		t.Fatal(err)
	}
	if _, err := client.ObserveWindow(ctx, ObserveWindowInput{
		SessionID: "session_fixture", WindowRef: "window_fixture", IncludeScreenshot: true,
		Accessibility: AccessibilityInteractive,
	}, timeout); err != nil {
		t.Fatal(err)
	}
	if _, err := client.ReadClipboard(ctx, "session_fixture", timeout); err != nil {
		t.Fatal(err)
	}
	if _, _, err := client.WriteClipboard(ctx, ClipboardWriteInput{
		SessionID: "session_fixture", ClipboardRef: "clipboard_fixture",
		Text: NewSensitiveText("replacement fixture"),
	}, timeout); err != nil {
		t.Fatal(err)
	}
	if _, _, err := client.PerformAction(ctx, PerformActionInput{
		SessionID: "session_fixture", WindowRef: "window_fixture", ObservationID: "observation_fixture",
		Action: FocusWindow{},
	}, timeout); err != nil {
		t.Fatal(err)
	}
	if _, err := client.VerifyState(ctx, VerifyStateInput{
		SessionID: "session_fixture", WindowRef: "window_fixture",
		Predicate: ElementStateMatches{
			Role: stringPointer("checkbox"), Name: stringPointer("Remember me"),
			State: ElementStateChecked, Value: true,
		},
	}, timeout); err != nil {
		t.Fatal(err)
	}
	if _, err := client.ReconcileMutation(ctx, "fixture-action", 0, timeout); err != nil {
		t.Fatal(err)
	}

	if len(transport.requests) != len(requests) {
		t.Fatalf("sent %d requests, want %d", len(transport.requests), len(requests))
	}
	for index := range requests {
		assertEquivalentJSON(t, transport.requests[index], requests[index])
	}
}

func stringPointer(value string) *string { return &value }

func TestEveryStableErrorFixtureDecodes(t *testing.T) {
	for _, raw := range loadRawArray(t, "errors.json") {
		var value CUAError
		if err := decodeStrict(raw, &value); err != nil {
			t.Fatalf("decode error fixture: %v", err)
		}
		if err := validateCUAError(&value); err != nil {
			t.Fatal(err)
		}
	}
}

func TestClosedEnumsAndUnknownFieldsFail(t *testing.T) {
	var platform Platform
	if err := json.Unmarshal([]byte(`"linux"`), &platform); err == nil {
		t.Fatal("unknown platform decoded")
	}
	var capabilities DriverCapabilities
	if err := decodeStrict([]byte(`{"protocol_version":"nexus.cua.v1","runtime_version":"x","platform":"macos","capture_modes":[],"accessibility_tree":false,"input_routes":[],"actions":[],"extra":true}`), &capabilities); err == nil {
		t.Fatal("unknown field decoded")
	}
	var output ActionOutput
	if err := decodeStrict([]byte(`{"delivery_mode":"system","dispatched":true,"effect_status":"confirmed","evidence_kind":"dispatch_acceptance","foreground_restoration":"not_requested","observation_invalidated":true}`), &output); err == nil {
		t.Fatal("incoherent action effect evidence decoded")
	}
	var restoration ForegroundRestorationStatus
	if err := json.Unmarshal([]byte(`"future_status"`), &restoration); err == nil {
		t.Fatal("unknown foreground restoration status decoded")
	}
	if err := validateManifest(CapabilityManifest{
		Mode: PermissionBounded, ApplicationRefs: []DiscoveryRef{"discovery_fixture"},
		RestoreForegroundAfterInput: true, TTLSeconds: 60,
	}); err == nil {
		t.Fatal("foreground restoration decoded without foreground authority")
	}
}

func TestSensitiveTextAlwaysFormatsRedacted(t *testing.T) {
	secret := NewSensitiveText("do-not-print")
	if fmt.Sprint(secret) != "[REDACTED]" || fmt.Sprintf("%#v", secret) != "SensitiveText([REDACTED])" {
		t.Fatal("sensitive text formatting was not redacted")
	}
	payload, err := json.Marshal(secret)
	if err != nil {
		t.Fatal(err)
	}
	if string(payload) != `"do-not-print"` {
		t.Fatalf("unexpected wire text %s", payload)
	}
	var decoded SensitiveText
	if err := json.Unmarshal(payload, &decoded); err != nil || decoded.Expose() != "do-not-print" {
		t.Fatalf("sensitive text did not round-trip: %v", err)
	}
}

func TestSelectApplicationPrefersExactAndRejectsAmbiguity(t *testing.T) {
	applications := []DiscoveredApplication{
		{Name: "AutoFill (Fixture)", ApplicationID: "com.example.helper"},
		{Name: "Fixture", ApplicationID: "dev.example.fixture"},
	}
	selected, err := SelectApplication(applications, "Fixture")
	if err != nil {
		t.Fatal(err)
	}
	if selected.ApplicationID != "dev.example.fixture" {
		t.Fatalf("selected %q, want exact fixture", selected.ApplicationID)
	}
	if _, err := SelectApplication(applications, "example"); err == nil {
		t.Fatal("ambiguous substring selected an arbitrary application")
	}
	if _, err := SelectApplication(applications, "Missing"); err == nil {
		t.Fatal("missing application selector did not fail")
	}
}

func TestSelectWindowDoesNotDependOnPlatformOrdering(t *testing.T) {
	windows := []WindowSummary{
		{WindowRef: "window_helper", Title: "", Minimized: true},
		{WindowRef: "window_fixture", Title: "Fixture · Generation 1"},
		{WindowRef: "window_help", Title: "Fixture Help"},
	}
	selected, err := SelectWindow(windows, "window_fixture")
	if err != nil {
		t.Fatal(err)
	}
	if selected.Title != "Fixture · Generation 1" {
		t.Fatalf("selected by ref %q, want fixture window", selected.Title)
	}
	selected, err = SelectWindow(windows, "Generation 1")
	if err != nil {
		t.Fatal(err)
	}
	if selected.Title != "Fixture · Generation 1" {
		t.Fatalf("selected %q, want fixture window", selected.Title)
	}
	if _, err := SelectWindow(windows, "Fixture"); err == nil {
		t.Fatal("ambiguous window substring selected an arbitrary window")
	}
}

func TestClientAndActionRequestFormattingHidePrivateState(t *testing.T) {
	client := &Client{token: "do-not-print"}
	request := &ActionRequest{
		requestID: "request_fixture",
		command: commandEnvelope{Operation: "perform_action", Input: map[string]any{
			"text": "do-not-print",
		}},
	}
	if value := fmt.Sprintf("%#v %#v", client, request); bytes.Contains([]byte(value), []byte("do-not-print")) {
		t.Fatal("private client or action state crossed formatting boundary")
	}
}

func TestFrameBoundaryFixture(t *testing.T) {
	path := filepath.Join("..", "..", "fixtures", "compatibility", "nexus.cua.v1", "frame-boundaries.json")
	payload, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var fixture struct {
		ConfiguredMaxBytes uint32 `json:"configured_max_bytes"`
	}
	if err := json.Unmarshal(payload, &fixture); err != nil {
		t.Fatal(err)
	}
	var exact bytes.Buffer
	if err := binary.Write(&exact, binary.BigEndian, fixture.ConfiguredMaxBytes); err != nil {
		t.Fatal(err)
	}
	exact.Write(bytes.Repeat([]byte{'x'}, int(fixture.ConfiguredMaxBytes)))
	decoded, err := readFrame(&exact, fixture.ConfiguredMaxBytes)
	if err != nil || len(decoded) != int(fixture.ConfiguredMaxBytes) {
		t.Fatalf("exact frame failed: bytes=%d error=%v", len(decoded), err)
	}
	var oversized bytes.Buffer
	if err := binary.Write(&oversized, binary.BigEndian, fixture.ConfiguredMaxBytes+1); err != nil {
		t.Fatal(err)
	}
	if _, err := readFrame(&oversized, fixture.ConfiguredMaxBytes); err == nil {
		t.Fatal("oversized frame was accepted")
	}
}

type reconcileTransport struct {
	requests [][]byte
	failOnce bool
}

func (transport *reconcileTransport) RoundTrip(_ context.Context, payload []byte, _ uint32) ([]byte, error) {
	transport.requests = append(transport.requests, append([]byte(nil), payload...))
	if transport.failOnce {
		transport.failOnce = false
		return nil, context.DeadlineExceeded
	}
	var request requestEnvelope
	if err := json.Unmarshal(payload, &request); err != nil {
		return nil, err
	}
	result := map[string]any{
		"result_type": "action_performed", "data": map[string]any{
			"delivery_mode": "semantic", "dispatched": true,
			"effect_status": "unverified", "evidence_kind": "dispatch_acceptance",
			"foreground_restoration":  "not_requested",
			"observation_invalidated": true,
		},
	}
	if request.Command.Operation == "launch_application" {
		result = map[string]any{
			"result_type": "application_launched", "data": map[string]any{
				"application": map[string]any{
					"discovery_ref": "discovery_launched", "name": "Fixture",
					"application_id": "dev.nexus.fixture", "foreground": true,
					"provenance": map[string]any{
						"platform": "macos", "bundle_id": "dev.nexus.fixture",
						"executable_path": "/Applications/Fixture.app/Contents/MacOS/Fixture",
						"signing_team_id": nil, "designated_requirement": nil,
					},
					"expires_at": "2026-08-25T12:00:30Z",
				},
				"delivery_mode": "foreground", "dispatched": true,
			},
		}
	}
	return json.Marshal(map[string]any{
		"protocol_version": ProtocolVersion, "request_id": request.RequestID,
		"outcome": map[string]any{"status": "success", "result": result},
	})
}

func TestMutationReconciliationReusesIdentityAndOnlyExtendsWait(t *testing.T) {
	transport := &reconcileTransport{failOnce: true}
	client := &Client{
		transport: transport, token: "x", maxFrameBytes: defaultMaxFrameBytes,
		reconciliationHorizon: defaultReconcileHorizon, now: time.Now,
		newRequestID: func() (RequestID, error) { return "request_reconcile", nil },
	}
	_, request, err := client.PerformAction(context.Background(), PerformActionInput{
		SessionID: "session", WindowRef: "window", ObservationID: "observation", Action: FocusWindow{},
	}, time.Second)
	var indeterminate *MutationIndeterminateError
	if !errors.As(err, &indeterminate) {
		t.Fatalf("expected indeterminate error, got %v", err)
	}
	if _, err := client.ReconcileAction(context.Background(), request, 500*time.Millisecond); err == nil {
		t.Fatal("shorter reconciliation wait accepted")
	}
	result, err := client.ReconcileAction(context.Background(), request, 2*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	if !result.Dispatched {
		t.Fatal("reconciled result did not dispatch")
	}
	if len(transport.requests) != 2 {
		t.Fatalf("got %d transport attempts", len(transport.requests))
	}
	var first, second requestEnvelope
	if err := json.Unmarshal(transport.requests[0], &first); err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(transport.requests[1], &second); err != nil {
		t.Fatal(err)
	}
	if first.RequestID != second.RequestID || first.Command.Operation != second.Command.Operation {
		t.Fatal("reconciliation changed request identity")
	}
	if first.TimeoutMS >= second.TimeoutMS {
		t.Fatal("reconciliation did not extend wire wait")
	}
}

func TestLaunchReconciliationReusesIdentityAndOnlyExtendsWait(t *testing.T) {
	transport := &reconcileTransport{failOnce: true}
	client := &Client{
		transport: transport, token: "x", maxFrameBytes: defaultMaxFrameBytes,
		reconciliationHorizon: defaultReconcileHorizon, now: time.Now,
		newRequestID: func() (RequestID, error) { return "request_launch_reconcile", nil },
	}
	_, request, err := client.LaunchApplication(context.Background(), LaunchApplicationInput{
		LaunchRef: "launch_fixture", AllowForegroundActivation: true,
	}, time.Second)
	var indeterminate *LaunchIndeterminateError
	if !errors.As(err, &indeterminate) {
		t.Fatalf("expected launch indeterminate error, got %v", err)
	}
	if _, err := client.ReconcileLaunch(context.Background(), request, 500*time.Millisecond); err == nil {
		t.Fatal("shorter launch reconciliation wait accepted")
	}
	result, err := client.ReconcileLaunch(context.Background(), request, 2*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	if !result.Dispatched || result.Application.ApplicationID != "dev.nexus.fixture" {
		t.Fatal("reconciled launch result is incomplete")
	}
	if len(transport.requests) != 2 {
		t.Fatalf("got %d transport attempts", len(transport.requests))
	}
	var first, second requestEnvelope
	if err := json.Unmarshal(transport.requests[0], &first); err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(transport.requests[1], &second); err != nil {
		t.Fatal(err)
	}
	if first.RequestID != second.RequestID || !reflect.DeepEqual(first.Command, second.Command) {
		t.Fatal("launch reconciliation changed request identity or command")
	}
	if first.TimeoutMS >= second.TimeoutMS {
		t.Fatal("launch reconciliation did not extend wire wait")
	}
}

func loadRawArray(t *testing.T, name string) []json.RawMessage {
	t.Helper()
	path := filepath.Join("..", "..", "fixtures", "compatibility", "nexus.cua.v1", name)
	payload, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var values []json.RawMessage
	if err := json.Unmarshal(payload, &values); err != nil {
		t.Fatal(err)
	}
	return values
}

func assertEquivalentJSON(t *testing.T, actual, expected []byte) {
	t.Helper()
	var actualValue, expectedValue any
	if err := json.Unmarshal(actual, &actualValue); err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(expected, &expectedValue); err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(actualValue, expectedValue) {
		t.Fatalf("JSON mismatch\nactual:   %s\nexpected: %s", actual, expected)
	}
}
