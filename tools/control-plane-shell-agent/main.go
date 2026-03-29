package main

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"log"
	"net"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"syscall"
	"time"
)

const (
	protocolVersion         = 2
	defaultSocketPath       = "/tmp/phantun-cp/agent.sock"
	defaultMaxScriptTimeout = 10 * time.Second
	socketFileMode          = 0o660
	maxLineSize             = 1024 * 1024
	maxOutputSize           = 16 * 1024
	maxResponseMessageSize  = 2048
)

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
	Peer4  *string `json:"peer4"`
	Peer6  *string `json:"peer6"`
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

type server struct {
	baseCtx          context.Context
	scriptPath       string
	maxScriptTimeout time.Duration
	execMu           sync.Mutex
}

type scriptResult struct {
	success  bool
	message  *string
	duration time.Duration
	stdout   string
	stderr   string
}

type limitedBuffer struct {
	max       int
	data      []byte
	truncated bool
}

var connCounter uint64

func main() {
	socketPath := flag.String("socket", defaultSocketPath, "UNIX socket path to listen on")
	scriptPath := flag.String("script", "", "Shell script path to execute for every control-plane request")
	maxScriptTimeout := flag.Duration("max-script-timeout", defaultMaxScriptTimeout, "Upper bound for script execution time")
	flag.Parse()

	if *scriptPath == "" {
		log.Fatal("--script is required")
	}
	if *maxScriptTimeout <= 0 {
		log.Fatal("--max-script-timeout must be greater than zero")
	}
	if err := validateScript(*scriptPath); err != nil {
		log.Fatalf("invalid --script: %v", err)
	}

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

	if err := os.Chmod(*socketPath, socketFileMode); err != nil {
		log.Fatalf("chmod socket: %v", err)
	}

	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()

	go func() {
		<-ctx.Done()
		_ = ln.Close()
	}()

	srv := &server{
		baseCtx:          ctx,
		scriptPath:       *scriptPath,
		maxScriptTimeout: *maxScriptTimeout,
	}

	log.Printf("control-plane shell agent listening on %s", *socketPath)
	log.Printf("script: %s", *scriptPath)
	log.Printf("max script timeout: %v", *maxScriptTimeout)

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
		go srv.handleConn(id, conn)
	}
}

func validateScript(scriptPath string) error {
	info, err := os.Stat(scriptPath)
	if err != nil {
		return err
	}
	if info.IsDir() {
		return fmt.Errorf("%s is a directory", scriptPath)
	}
	return nil
}

func (s *server) handleConn(id uint64, conn net.Conn) {
	defer conn.Close()
	log.Printf("[conn:%d] connected", id)
	defer log.Printf("[conn:%d] closed", id)

	go func() {
		<-s.baseCtx.Done()
		_ = conn.Close()
	}()

	scanner := bufio.NewScanner(conn)
	scanner.Buffer(make([]byte, 0, 64*1024), maxLineSize)

	for scanner.Scan() {
		line := scanner.Bytes()
		log.Printf("[conn:%d] <= %s", id, string(line))

		var req controlRequest
		if err := json.Unmarshal(line, &req); err != nil {
			log.Printf("[conn:%d] invalid request json: %v", id, err)
			return
		}

		resp := s.handleRequest(&req)

		payload, err := json.Marshal(resp)
		if err != nil {
			log.Printf("[conn:%d] response marshal error: %v", id, err)
			return
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

func (s *server) handleRequest(req *controlRequest) controlResponse {
	if err := validateRequest(req); err != nil {
		log.Printf(
			"request rejected session_id=%q id=%d phase=%q: %v",
			req.SessionID,
			req.ID,
			req.Phase,
			err,
		)
		return failureResponse(req, err.Error())
	}

	result := s.runScript(req)
	log.Printf(
		"request phase=%s session_id=%s id=%d success=%t duration=%s",
		req.Phase,
		req.SessionID,
		req.ID,
		result.success,
		result.duration,
	)
	if result.stdout != "" {
		log.Printf("request phase=%s session_id=%s id=%d stdout=%q", req.Phase, req.SessionID, req.ID, result.stdout)
	}
	if result.stderr != "" {
		log.Printf("request phase=%s session_id=%s id=%d stderr=%q", req.Phase, req.SessionID, req.ID, result.stderr)
	}

	return controlResponse{
		Version:   protocolVersion,
		Kind:      "response",
		SessionID: req.SessionID,
		ID:        req.ID,
		Success:   result.success,
		Message:   result.message,
	}
}

func validateRequest(req *controlRequest) error {
	if req.Version != protocolVersion {
		return fmt.Errorf("unsupported protocol version %d", req.Version)
	}
	if req.Kind != "request" {
		return fmt.Errorf("unexpected kind %q", req.Kind)
	}
	if req.SessionID == "" {
		return errors.New("missing session_id")
	}
	if req.Phase == "" {
		return errors.New("missing phase")
	}
	if req.Payload.State == "" {
		return errors.New("missing payload.state")
	}
	if req.Payload.Mode == "" {
		return errors.New("missing payload.mode")
	}
	return nil
}

func failureResponse(req *controlRequest, message string) controlResponse {
	return controlResponse{
		Version:   protocolVersion,
		Kind:      "response",
		SessionID: req.SessionID,
		ID:        req.ID,
		Success:   false,
		Message:   stringPtr(limitMessage(message)),
	}
}

func (s *server) runScript(req *controlRequest) scriptResult {
	timeout := effectiveTimeout(req.DeadlineMS, s.maxScriptTimeout)
	ctx, cancel := context.WithTimeout(s.baseCtx, timeout)
	defer cancel()

	cmd := exec.CommandContext(ctx, "/bin/sh", s.scriptPath)
	cmd.Env = buildEnv(req)
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}

	stdout := &limitedBuffer{max: maxOutputSize}
	stderr := &limitedBuffer{max: maxOutputSize}
	cmd.Stdout = stdout
	cmd.Stderr = stderr

	start := time.Now()
	s.execMu.Lock()
	defer s.execMu.Unlock()

	if err := cmd.Start(); err != nil {
		msg := fmt.Sprintf("start script: %v", err)
		return scriptResult{
			success:  false,
			message:  stringPtr(limitMessage(msg)),
			duration: time.Since(start),
			stdout:   stdout.String(),
			stderr:   stderr.String(),
		}
	}

	waitCh := make(chan error, 1)
	go func() {
		waitCh <- cmd.Wait()
	}()

	select {
	case err := <-waitCh:
		return classifyScriptResult(err, time.Since(start), stdout.String(), stderr.String())
	case <-ctx.Done():
		killProcessGroup(cmd.Process.Pid)
		<-waitCh

		msg := shutdownMessage(ctx.Err(), timeout)
		return scriptResult{
			success:  false,
			message:  stringPtr(limitMessage(msg)),
			duration: time.Since(start),
			stdout:   stdout.String(),
			stderr:   stderr.String(),
		}
	}
}

func classifyScriptResult(err error, duration time.Duration, stdout string, stderr string) scriptResult {
	if err == nil {
		return scriptResult{
			success:  true,
			message:  nil,
			duration: duration,
			stdout:   stdout,
			stderr:   stderr,
		}
	}

	var exitErr *exec.ExitError
	if errors.As(err, &exitErr) {
		msg := preferredMessage(stderr, stdout)
		if msg == "" {
			msg = fmt.Sprintf("script exited with code %d", exitErr.ExitCode())
		} else {
			msg = fmt.Sprintf("exit code %d: %s", exitErr.ExitCode(), msg)
		}
		return scriptResult{
			success:  false,
			message:  stringPtr(limitMessage(msg)),
			duration: duration,
			stdout:   stdout,
			stderr:   stderr,
		}
	}

	msg := fmt.Sprintf("script failed: %v", err)
	return scriptResult{
		success:  false,
		message:  stringPtr(limitMessage(msg)),
		duration: duration,
		stdout:   stdout,
		stderr:   stderr,
	}
}

func effectiveTimeout(deadlineMS uint64, maxScriptTimeout time.Duration) time.Duration {
	if deadlineMS == 0 {
		return maxScriptTimeout
	}
	maxMillis := uint64(maxScriptTimeout / time.Millisecond)
	if maxMillis == 0 || deadlineMS > maxMillis {
		return maxScriptTimeout
	}
	return time.Duration(deadlineMS) * time.Millisecond
}

func buildEnv(req *controlRequest) []string {
	env := make([]string, 0, len(os.Environ())+16)
	for _, item := range os.Environ() {
		key, _, found := strings.Cut(item, "=")
		if !found {
			continue
		}
		if strings.HasPrefix(key, "PHANTUN_") {
			continue
		}
		env = append(env, item)
	}

	appendEnv := func(key, value string) {
		env = append(env, key+"="+value)
	}
	appendEnvOpt := func(key string, value *string) {
		if value != nil {
			appendEnv(key, *value)
		}
	}

	appendEnv("PHANTUN_PROTOCOL_VERSION", strconv.Itoa(req.Version))
	appendEnv("PHANTUN_KIND", req.Kind)
	appendEnv("PHANTUN_SESSION_ID", req.SessionID)
	appendEnv("PHANTUN_REQUEST_ID", strconv.FormatUint(req.ID, 10))
	appendEnv("PHANTUN_PHASE", req.Phase)
	appendEnv("PHANTUN_DEADLINE_MS", strconv.FormatUint(req.DeadlineMS, 10))
	appendEnv("PHANTUN_STATE", req.Payload.State)
	appendEnv("PHANTUN_MODE", req.Payload.Mode)
	appendEnvOpt("PHANTUN_REASON", req.Payload.Reason)
	appendEnvOpt("PHANTUN_LOCAL", req.Payload.Local)
	appendEnvOpt("PHANTUN_REMOTE", req.Payload.Remote)
	appendEnvOpt("PHANTUN_DEV", req.Payload.Dev)
	appendEnvOpt("PHANTUN_ADDR4", req.Payload.Addr4)
	appendEnvOpt("PHANTUN_ADDR6", req.Payload.Addr6)
	appendEnvOpt("PHANTUN_PEER4", req.Payload.Peer4)
	appendEnvOpt("PHANTUN_PEER6", req.Payload.Peer6)
	if req.Payload.MTU != nil {
		appendEnv("PHANTUN_MTU", strconv.FormatUint(uint64(*req.Payload.MTU), 10))
	}

	return env
}

func preferredMessage(stderr string, stdout string) string {
	if msg := strings.TrimSpace(stderr); msg != "" {
		return msg
	}
	return strings.TrimSpace(stdout)
}

func shutdownMessage(err error, timeout time.Duration) string {
	if errors.Is(err, context.Canceled) {
		return "script canceled by agent shutdown"
	}
	return fmt.Sprintf("script timed out after %s", timeout)
}

func limitMessage(message string) string {
	message = strings.TrimSpace(message)
	if len(message) <= maxResponseMessageSize {
		return message
	}
	if maxResponseMessageSize <= len("...") {
		return message[:maxResponseMessageSize]
	}
	return message[:maxResponseMessageSize-3] + "..."
}

func stringPtr(value string) *string {
	return &value
}

func killProcessGroup(pid int) {
	if pid <= 0 {
		return
	}
	if err := syscall.Kill(-pid, syscall.SIGKILL); err != nil && !errors.Is(err, syscall.ESRCH) {
		log.Printf("kill process group %d: %v", pid, err)
	}
}

func (b *limitedBuffer) Write(p []byte) (int, error) {
	if b.max <= len(b.data) {
		b.truncated = true
		return len(p), nil
	}

	remaining := b.max - len(b.data)
	if len(p) > remaining {
		b.data = append(b.data, p[:remaining]...)
		b.truncated = true
		return len(p), nil
	}

	b.data = append(b.data, p...)
	return len(p), nil
}

func (b *limitedBuffer) String() string {
	value := strings.TrimSpace(string(b.data))
	if b.truncated && value != "" {
		return value + " [truncated]"
	}
	if b.truncated {
		return "[truncated]"
	}
	return value
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
