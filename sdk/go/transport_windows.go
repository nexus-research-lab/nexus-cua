//go:build windows

package nexuscua

import (
	"context"
	"time"

	"github.com/Microsoft/go-winio"
)

func (transport nativeTransport) RoundTrip(ctx context.Context, request []byte, limit uint32) ([]byte, error) {
	timeout := 30 * time.Second
	if deadline, ok := ctx.Deadline(); ok {
		timeout = time.Until(deadline)
		if timeout <= 0 {
			return nil, context.DeadlineExceeded
		}
	}
	connection, err := winio.DialPipeContext(ctx, transport.endpoint)
	if err != nil {
		return nil, err
	}
	defer connection.Close()
	if deadline, ok := ctx.Deadline(); ok {
		if err := connection.SetDeadline(deadline); err != nil {
			return nil, err
		}
	} else {
		if err := connection.SetDeadline(time.Now().Add(timeout)); err != nil {
			return nil, err
		}
	}
	if err := writeFrame(connection, request, limit); err != nil {
		return nil, err
	}
	return readFrame(connection, limit)
}
