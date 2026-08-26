//go:build unix

package nexuscua

import (
	"context"
	"net"
)

func (transport nativeTransport) RoundTrip(ctx context.Context, request []byte, limit uint32) ([]byte, error) {
	connection, err := (&net.Dialer{}).DialContext(ctx, "unix", transport.endpoint)
	if err != nil {
		return nil, err
	}
	defer connection.Close()
	if deadline, ok := ctx.Deadline(); ok {
		if err := connection.SetDeadline(deadline); err != nil {
			return nil, err
		}
	}
	if err := writeFrame(connection, request, limit); err != nil {
		return nil, err
	}
	return readFrame(connection, limit)
}
