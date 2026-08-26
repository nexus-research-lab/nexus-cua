//go:build !unix && !windows

package nexuscua

import (
	"context"
	"fmt"
)

func (transport nativeTransport) RoundTrip(context.Context, []byte, uint32) ([]byte, error) {
	return nil, fmt.Errorf("nexus-cua: local transport is unsupported on this platform")
}
