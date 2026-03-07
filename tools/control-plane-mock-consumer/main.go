package main

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"flag"
	"io"
	"log"
	"net"
	"os"
	"os/signal"
	"path/filepath"
	"time"
	"sync/atomic"
	"syscall"
)

type controlMessage struct {
	Type string `json:"type"`
	ID   string `json:"id"`
	Data struct {
		State string `json:"state"`
	} `json:"data"`
}

type ackMessage struct {
	Type string `json:"type"`
	ID   string `json:"id"`
}

var connCounter uint64

func main() {
	socketPath := flag.String("socket", "/tmp/phantun-cp/agent.sock", "UNIX socket path to listen on")
	ackState := flag.String("ack-state", "stopping", "State value that triggers ACK")
	ackType := flag.String("ack-type", "", "Optional message type filter for ACK (for example: event). Empty means no type filter")
	ackDelay := flag.Duration("ack-delay", 0, "Optional delay before sending ACK (for timeout testing)")
	noAck := flag.Bool("no-ack", false, "Disable ACK replies")
	flag.Parse()

	if err := os.MkdirAll(filepath.Dir(*socketPath), 0o755); err != nil {
		log.Fatalf("create socket dir: %v", err)
	}

	// Remove stale socket path if it exists.
	if err := os.Remove(*socketPath); err != nil && !errors.Is(err, os.ErrNotExist) {
		log.Fatalf("remove stale socket: %v", err)
	}

	ln, err := net.Listen("unix", *socketPath)
	if err != nil {
		log.Fatalf("listen unix socket: %v", err)
	}
	defer func() {
		_ = ln.Close()
		_ = os.Remove(*socketPath)
	}()

	log.Printf("mock consumer listening on %s", *socketPath)
	log.Printf(
		"ack state: %q, ack type filter: %q, ack delay: %v, ack enabled: %v",
		*ackState, *ackType, *ackDelay, !*noAck,
	)

	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()

	go func() {
		<-ctx.Done()
		_ = ln.Close()
	}()

	for {
		conn, err := ln.Accept()
		if err != nil {
			if ctx.Err() != nil {
				log.Printf("shutdown")
				return
			}
			log.Printf("accept error: %v", err)
			continue
		}

		id := atomic.AddUint64(&connCounter, 1)
		go handleConn(id, conn, *ackState, *ackType, *ackDelay, !*noAck)
	}
}

func handleConn(
	id uint64,
	conn net.Conn,
	ackState string,
	ackType string,
	ackDelay time.Duration,
	ackEnabled bool,
) {
	defer conn.Close()
	log.Printf("[conn:%d] connected from %s", id, conn.RemoteAddr())
	defer log.Printf("[conn:%d] closed", id)

	scanner := bufio.NewScanner(conn)
	// Default scanner token is 64K; raise for safety.
	buf := make([]byte, 0, 64*1024)
	scanner.Buffer(buf, 1024*1024)

	for scanner.Scan() {
		line := scanner.Text()
		log.Printf("[conn:%d] <= %s", id, line)

		if !ackEnabled {
			continue
		}

		var msg controlMessage
		if err := json.Unmarshal([]byte(line), &msg); err != nil {
			log.Printf("[conn:%d] parse warning: %v", id, err)
			continue
		}

		if msg.Data.State != ackState || msg.ID == "" {
			continue
		}
		if ackType != "" && msg.Type != ackType {
			continue
		}

		if ackDelay > 0 {
			time.Sleep(ackDelay)
		}

		ack := ackMessage{
			Type: "ack",
			ID:   msg.ID,
		}
		payload, err := json.Marshal(ack)
		if err != nil {
			log.Printf("[conn:%d] ack marshal error: %v", id, err)
			continue
		}
		payload = append(payload, '\n')

		if err := writeAll(conn, payload); err != nil {
			if !errors.Is(err, io.EOF) {
				log.Printf("[conn:%d] ack write error: %v", id, err)
			}
			return
		}
		log.Printf("[conn:%d] => %s", id, string(payload[:len(payload)-1]))
	}

	if err := scanner.Err(); err != nil && !errors.Is(err, io.EOF) {
		log.Printf("[conn:%d] read error: %v", id, err)
	}
}

func writeAll(conn net.Conn, payload []byte) error {
	written := 0
	for written < len(payload) {
		n, err := conn.Write(payload[written:])
		if err != nil {
			return err
		}
		if n == 0 {
			return io.ErrUnexpectedEOF
		}
		written += n
	}
	return nil
}
