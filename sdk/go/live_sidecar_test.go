package nexuscua

import (
	"context"
	"os"
	"testing"
	"time"
)

func TestLiveSidecar(t *testing.T) {
	endpoint := os.Getenv("NEXUS_CUA_LIVE_ENDPOINT")
	tokenFile := os.Getenv("NEXUS_CUA_LIVE_TOKEN_FILE")
	if endpoint == "" || tokenFile == "" {
		t.Skip("set NEXUS_CUA_LIVE_ENDPOINT and NEXUS_CUA_LIVE_TOKEN_FILE")
	}
	client, err := NewClient(Config{Endpoint: endpoint, TokenFile: tokenFile})
	if err != nil {
		t.Fatal(err)
	}
	ctx := context.Background()
	capabilities, err := client.GetCapabilities(ctx, 10*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	if capabilities.ProtocolVersion != ProtocolVersion {
		t.Fatalf("unexpected protocol %q", capabilities.ProtocolVersion)
	}
	if _, err := client.GetPermissionStatus(ctx, 10*time.Second); err != nil {
		t.Fatal(err)
	}
	discovery, err := client.DiscoverApplications(ctx, 10*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	applicationMatch := os.Getenv("NEXUS_CUA_LIVE_APPLICATION_MATCH")
	if applicationMatch == "" {
		return
	}
	selected, err := SelectApplication(discovery.Applications, applicationMatch)
	if err != nil {
		t.Fatal(err)
	}
	manifest := CapabilityManifest{
		Mode: PermissionReadOnly, ApplicationRefs: []DiscoveryRef{selected.DiscoveryRef}, TTLSeconds: 60,
	}
	mutation := os.Getenv("NEXUS_CUA_LIVE_MUTATION") == "1"
	if mutation {
		manifest.Mode = PermissionBounded
		manifest.AllowedActions = []ActionKind{ActionInvokeElement}
	}
	session, err := client.OpenSession(ctx, OpenSessionInput{Manifest: manifest}, 10*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	defer func() {
		if err := client.CloseSession(ctx, session.SessionID, 10*time.Second); err != nil {
			t.Errorf("close session: %v", err)
		}
	}()
	windows, err := client.ListWindows(ctx, session.SessionID, nil, 10*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	windowMatch := os.Getenv("NEXUS_CUA_LIVE_WINDOW_MATCH")
	if windowMatch == "" {
		windowMatch = "Nexus CUA Native Fixture · Generation "
	}
	window, err := SelectWindow(windows, windowMatch)
	if err != nil {
		t.Fatal(err)
	}
	observation, err := client.ObserveWindow(ctx, ObserveWindowInput{
		SessionID: session.SessionID, WindowRef: window.WindowRef,
		IncludeScreenshot: false, Accessibility: AccessibilityInteractive,
	}, 20*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	if !mutation {
		return
	}
	var increment *AccessibilityElement
	for index := range observation.Elements {
		element := &observation.Elements[index]
		if element.Name == "Increment Counter" {
			increment = element
			break
		}
	}
	if increment == nil {
		t.Fatal("fixture increment button was not observed")
	}
	if _, _, err := client.PerformAction(ctx, PerformActionInput{
		SessionID: session.SessionID, WindowRef: window.WindowRef,
		ObservationID: observation.ObservationID,
		Action:        InvokeElement{ElementRef: increment.ElementRef},
	}, 20*time.Second); err != nil {
		t.Fatal(err)
	}
	if _, err := client.ObserveWindow(ctx, ObserveWindowInput{
		SessionID: session.SessionID, WindowRef: window.WindowRef,
		IncludeScreenshot: false, Accessibility: AccessibilityInteractive,
	}, 20*time.Second); err != nil {
		t.Fatal(err)
	}
}
