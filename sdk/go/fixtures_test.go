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
	if len(requests) != 10 || len(results) != 10 {
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
	if _, err := client.OpenSession(ctx, OpenSessionInput{Manifest: CapabilityManifest{
		Mode: PermissionBounded, ApplicationRefs: []DiscoveryRef{"discovery_fixture"},
		AllowedActions:       []ActionKind{ActionFocusWindow, ActionInvokeElement, ActionClickPoint},
		AllowForegroundInput: true, TTLSeconds: 300,
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
	if _, err := client.ObserveWindow(ctx, ObserveWindowInput{
		SessionID: "session_fixture", WindowRef: "window_fixture", IncludeScreenshot: true,
		Accessibility: AccessibilityInteractive,
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
		Predicate: WindowTitleContains{Text: "Fixture"},
	}, timeout); err != nil {
		t.Fatal(err)
	}

	if len(transport.requests) != len(requests) {
		t.Fatalf("sent %d requests, want %d", len(transport.requests), len(requests))
	}
	for index := range requests {
		assertEquivalentJSON(t, transport.requests[index], requests[index])
	}
}

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
		{Title: "", Minimized: true},
		{Title: "Fixture · Generation 1"},
		{Title: "Fixture Help"},
	}
	selected, err := SelectWindow(windows, "Generation 1")
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
	return json.Marshal(map[string]any{
		"protocol_version": ProtocolVersion, "request_id": request.RequestID,
		"outcome": map[string]any{"status": "success", "result": map[string]any{
			"result_type": "action_performed", "data": map[string]any{
				"delivery_mode": "semantic", "dispatched": true, "observation_invalidated": true,
			},
		}},
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
