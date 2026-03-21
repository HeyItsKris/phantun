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
	"sync/atomic"
	"syscall"
	"time"
)

type stringList []string

func (s *stringList) String() string {
	return ""
}

func (s *stringList) Set(value string) error {
	*s = append(*s, value)
	return nil
}

type controlPayload struct {
	State  string  `json:"state"`
	Reason *string `json:"reason"`
	Mode   string  `json:"mode"`
	Local  *string `json:"local"`
	Remote *string `json:"remote"`
	Dev    *string `json:"dev"`
	MTU    *uint32 `json:"mtu"`
	Addr4  *string `json:"addr4"`
	Addr6  *string `json:"addr6"`
}

type controlRequest struct {
	Version    int            `json:"v"`
	Kind       string         `json:"kind"`
	SessionID  string         `json:"session_id"`
	ID         uint64         `json:"id"`
	Phase      string         `json:"phase"`
	DeadlineMS uint64         `json:"deadline_ms"`
	Payload    controlPayload `json:"payload"`
}

type controlResponse struct {
	Version   int     `json:"v"`
	Kind      string  `json:"kind"`
	SessionID string  `json:"session_id"`
	ID        uint64  `json:"id"`
	Success   bool    `json:"success"`
	Message   *string `json:"message"`
}

var connCounter uint64

func main() {
	socketPath := flag.String("socket", "/tmp/phantun-cp/agent.sock", "UNIX socket path to listen on")
	replyDelay := flag.Duration("reply-delay", 0, "Optional delay before sending a response")
	failureMessage := flag.String("failure-message", "rejected by mock agent", "Message used for failed responses")

	var failPhases stringList
	var dropPhases stringList
	flag.Var(&failPhases, "fail-phase", "Phase that should return success=false (repeatable)")
	flag.Var(&dropPhases, "drop-phase", "Phase that should close the connection without a response (repeatable)")
	flag.Parse()

	if err := os.MkdirAll(filepath.Dir(*socketPath), 0o755); err != nil {
		log.Fatalf("create socket dir: %v", err)
	}

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

	failSet := makeStringSet(failPhases)
	dropSet := makeStringSet(dropPhases)

	log.Printf("mock agent listening on %s", *socketPath)
	log.Printf("reply delay: %v", *replyDelay)
	log.Printf("fail phases: %v", failPhases)
	log.Printf("drop phases: %v", dropPhases)

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
		go handleConn(id, conn, *replyDelay, failSet, dropSet, *failureMessage)
	}
}

func handleConn(
	id uint64,
	conn net.Conn,
	replyDelay time.Duration,
	failPhases map[string]struct{},
	dropPhases map[string]struct{},
	failureMessage string,
) {
	defer conn.Close()
	log.Printf("[conn:%d] connected from %s", id, conn.RemoteAddr())
	defer log.Printf("[conn:%d] closed", id)

	scanner := bufio.NewScanner(conn)
	buf := make([]byte, 0, 64*1024)
	scanner.Buffer(buf, 1024*1024)

	for scanner.Scan() {
		line := scanner.Text()
		log.Printf("[conn:%d] <= %s", id, line)

		var req controlRequest
		if err := json.Unmarshal([]byte(line), &req); err != nil {
			log.Printf("[conn:%d] parse warning: %v", id, err)
			continue
		}

		if req.Kind != "request" {
			log.Printf("[conn:%d] ignoring non-request kind=%q", id, req.Kind)
			continue
		}

		if _, shouldDrop := dropPhases[req.Phase]; shouldDrop {
			log.Printf("[conn:%d] dropping connection on phase=%q", id, req.Phase)
			return
		}

		if replyDelay > 0 {
			time.Sleep(replyDelay)
		}

		success := true
		message := "accepted"
		if _, shouldFail := failPhases[req.Phase]; shouldFail {
			success = false
			message = failureMessage
		}

		resp := controlResponse{
			Version:   req.Version,
			Kind:      "response",
			SessionID: req.SessionID,
			ID:        req.ID,
			Success:   success,
			Message:   stringPtr(message),
		}

		payload, err := json.Marshal(resp)
		if err != nil {
			log.Printf("[conn:%d] response marshal error: %v", id, err)
			continue
		}
		payload = append(payload, '\n')

		if err := writeAll(conn, payload); err != nil {
			if !errors.Is(err, io.EOF) {
				log.Printf("[conn:%d] response write error: %v", id, err)
			}
			return
		}
		log.Printf("[conn:%d] => %s", id, string(payload[:len(payload)-1]))
	}

	if err := scanner.Err(); err != nil && !errors.Is(err, io.EOF) {
		log.Printf("[conn:%d] read error: %v", id, err)
	}
}

func makeStringSet(values []string) map[string]struct{} {
	out := make(map[string]struct{}, len(values))
	for _, value := range values {
		out[value] = struct{}{}
	}
	return out
}

func stringPtr(value string) *string {
	return &value
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
