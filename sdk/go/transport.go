package nexuscua

import (
	"context"
	"encoding/binary"
	"fmt"
	"io"
)

type roundTripper interface {
	RoundTrip(context.Context, []byte, uint32) ([]byte, error)
}

type nativeTransport struct{ endpoint string }

func writeFrame(writer io.Writer, payload []byte, limit uint32) error {
	if len(payload) == 0 || uint64(len(payload)) > uint64(limit) {
		return fmt.Errorf("nexus-cua: request frame exceeds configured bound")
	}
	var header [4]byte
	binary.BigEndian.PutUint32(header[:], uint32(len(payload)))
	if err := writeAll(writer, header[:]); err != nil {
		return err
	}
	return writeAll(writer, payload)
}

func readFrame(reader io.Reader, limit uint32) ([]byte, error) {
	var header [4]byte
	if _, err := io.ReadFull(reader, header[:]); err != nil {
		return nil, err
	}
	length := binary.BigEndian.Uint32(header[:])
	if length == 0 || length > limit {
		return nil, fmt.Errorf("nexus-cua: response frame length %d is outside configured bound", length)
	}
	payload := make([]byte, length)
	if _, err := io.ReadFull(reader, payload); err != nil {
		return nil, err
	}
	return payload, nil
}

func writeAll(writer io.Writer, payload []byte) error {
	for len(payload) > 0 {
		written, err := writer.Write(payload)
		if err != nil {
			return err
		}
		if written == 0 {
			return io.ErrShortWrite
		}
		payload = payload[written:]
	}
	return nil
}
