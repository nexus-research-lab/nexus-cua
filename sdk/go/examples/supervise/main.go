// Command supervise demonstrates host-owned sidecar lifecycle. Production
// hosts should replace the temporary state directory with an owner-private,
// durable location and apply the platform ACL policy documented by nexus-cua.
package main

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"flag"
	"fmt"
	"log"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"runtime"
	"time"

	nexuscua "github.com/nexus-research-lab/nexus-cua/sdk/go"
)

func main() {
	sidecar := flag.String("sidecar", "nexus-cua", "path to an already verified nexus-cua binary")
	flag.Parse()

	state, err := os.MkdirTemp("", "nexus-cua-supervisor-")
	if err != nil {
		log.Fatal(err)
	}
	defer os.RemoveAll(state)
	if err := os.Chmod(state, 0o700); err != nil {
		log.Fatal(err)
	}

	token := make([]byte, 32)
	if _, err := rand.Read(token); err != nil {
		log.Fatal(err)
	}
	tokenFile := filepath.Join(state, "token")
	if err := os.WriteFile(tokenFile, []byte(hex.EncodeToString(token)), 0o600); err != nil {
		log.Fatal(err)
	}
	endpoint := filepath.Join(state, "service.sock")
	if runtime.GOOS == "windows" {
		endpoint = `\\.\pipe\nexus-cua-supervisor-` + hex.EncodeToString(token[:8])
	}
	artifacts := filepath.Join(state, "artifacts")
	if err := os.Mkdir(artifacts, 0o700); err != nil {
		log.Fatal(err)
	}

	command := exec.Command(
		*sidecar, "serve", "--endpoint", endpoint,
		"--token-file", tokenFile, "--artifact-root", artifacts,
	)
	command.Stdout = os.Stderr
	command.Stderr = os.Stderr
	if err := command.Start(); err != nil {
		log.Fatal(err)
	}

	client, err := nexuscua.NewClient(nexuscua.Config{Endpoint: endpoint, TokenFile: tokenFile})
	if err != nil {
		_ = command.Process.Kill()
		log.Fatal(err)
	}
	if err := waitUntilReady(client); err != nil {
		_ = command.Process.Kill()
		log.Fatal(err)
	}
	fmt.Println("sidecar ready; press Ctrl-C to stop")

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt)
	defer stop()
	<-ctx.Done()
	if runtime.GOOS == "windows" {
		// A production Windows service supervisor should use its service/job
		// control path. os.Interrupt cannot deliver CTRL_C_EVENT reliably.
		_ = command.Process.Kill()
	} else {
		_ = command.Process.Signal(os.Interrupt)
	}
	if err := command.Wait(); err != nil {
		log.Printf("sidecar stopped: %v", err)
	}
}

func waitUntilReady(client *nexuscua.Client) error {
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		ctx, cancel := context.WithTimeout(context.Background(), 500*time.Millisecond)
		capabilities, err := client.GetCapabilities(ctx, 500*time.Millisecond)
		cancel()
		if err == nil && capabilities.ProtocolVersion == nexuscua.ProtocolVersion {
			return nil
		}
		time.Sleep(50 * time.Millisecond)
	}
	return fmt.Errorf("sidecar did not become ready before the supervisor deadline")
}
