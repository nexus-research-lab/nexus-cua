package main

import (
	"context"
	"flag"
	"fmt"
	"log"
	"time"

	nexuscua "github.com/nexus-research-lab/nexus-cua/sdk/go"
)

func main() {
	endpoint := flag.String("endpoint", "", "sidecar Unix socket or Windows named pipe")
	tokenFile := flag.String("token-file", "", "host-private sidecar token file")
	applicationMatch := flag.String("application", "", "application name or stable ID substring")
	flag.Parse()
	if *endpoint == "" || *tokenFile == "" || *applicationMatch == "" {
		flag.Usage()
		return
	}

	client, err := nexuscua.NewClient(nexuscua.Config{Endpoint: *endpoint, TokenFile: *tokenFile})
	if err != nil {
		log.Fatal(err)
	}
	ctx := context.Background()
	discovery, err := client.DiscoverApplications(ctx, 10*time.Second)
	if err != nil {
		log.Fatal(err)
	}
	selected, err := nexuscua.SelectApplication(discovery.Applications, *applicationMatch)
	if err != nil {
		log.Fatal(err)
	}

	session, err := client.OpenSession(ctx, nexuscua.OpenSessionInput{Manifest: nexuscua.CapabilityManifest{
		Mode: nexuscua.PermissionReadOnly, ApplicationRefs: []nexuscua.DiscoveryRef{selected.DiscoveryRef}, TTLSeconds: 60,
	}}, 10*time.Second)
	if err != nil {
		log.Fatal(err)
	}
	defer func() {
		if err := client.CloseSession(ctx, session.SessionID, 10*time.Second); err != nil {
			log.Printf("close session: %v", err)
		}
	}()

	windows, err := client.ListWindows(ctx, session.SessionID, nil, 10*time.Second)
	if err != nil {
		log.Fatal(err)
	}
	window, err := nexuscua.SelectWindow(windows, *applicationMatch)
	if err != nil {
		log.Fatal(err)
	}
	observation, err := client.ObserveWindow(ctx, nexuscua.ObserveWindowInput{
		SessionID: session.SessionID, WindowRef: window.WindowRef,
		IncludeScreenshot: true, Accessibility: nexuscua.AccessibilityInteractive,
	}, 20*time.Second)
	if err != nil {
		log.Fatal(err)
	}
	fmt.Printf("window=%q elements=%d screenshot=%t\n", window.Title, len(observation.Elements), observation.Screenshot != nil)
}
