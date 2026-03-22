# control-plane-mock-consumer

Minimal mock agent for Phantun `v2` control-plane testing.

## Run

```bash
go run ./tools/control-plane-mock-consumer \
  --socket /tmp/phantun-cp/agent.sock
```

## Behavior

1. Listens on UNIX socket (server side).
2. Prints each incoming NDJSON line.
3. Replies with a `v2` `response` message for each incoming `request`.
4. Keeps the connection open unless configured to drop on a phase.

## Options

```bash
go run ./tools/control-plane-mock-consumer --help
```

- `--socket`: UNIX socket listen path.
- `--reply-delay`: Delay response send, for timeout/path testing.
- `--fail-phase`: Return `success=false` for the given phase. Repeatable.
- `--drop-phase`: Close the connection without a response for the given phase. Repeatable.
- `--failure-message`: Message used in failed responses.

## Phases

The mock agent handles the `v2` control-plane request phases used by Phantun:

- `pre_start`
- `post_start`
- `sync_state`
- `pre_stop`
- `post_stop`

By default, every incoming `request` receives a successful `response` and the
connection stays open.

## Typical usage

Build the binary once:

```bash
go build -o main ./tools/control-plane-mock-consumer
```

Start the mock agent:

```bash
./main --socket /run/phantun-cp/agent.sock
```

Then point Phantun at the same socket path:

```bash
phantun-server ... --control-target /run/phantun-cp/agent.sock
```

or:

```bash
phantun-client ... --control-target /run/phantun-cp/agent.sock
```

## Test scenarios

### 1. Normal path

All phases succeed.

```bash
./main --socket /run/phantun-cp/agent.sock
```

Expected behavior:

1. Phantun connects successfully during startup.
2. `pre_start` and `post_start` both receive `success=true`.
3. On shutdown, `pre_stop` and `post_stop` both receive `success=true`.

### 2. Slow response

Delay every response to test timeout handling.

```bash
./main --socket /run/phantun-cp/agent.sock --reply-delay 2s
```

Expected behavior:

1. Requests are still answered.
2. If the delay exceeds Phantun's phase timeout, startup or shutdown will take
   the timeout path instead of waiting forever.

### 3. Reject a specific phase

Fail `pre_start`:

```bash
./main --socket /run/phantun-cp/agent.sock --fail-phase pre_start
```

Fail `post_start` with a custom message:

```bash
./main --socket /run/phantun-cp/agent.sock \
  --fail-phase post_start \
  --failure-message "post_start rejected by test agent"
```

Expected behavior:

1. The selected phase returns `success=false`.
2. Phantun treats that phase as a barrier failure.

### 4. Drop the connection on a specific phase

Drop on `post_start`:

```bash
./main --socket /run/phantun-cp/agent.sock --drop-phase post_start
```

Drop on `sync_state`:

```bash
./main --socket /run/phantun-cp/agent.sock --drop-phase sync_state
```

Expected behavior:

1. The agent closes the connection without sending a response.
2. Phantun enters its reconnect or failure handling path.

### 5. Combine behaviors

Example: fail `pre_stop`, and drop the connection on `post_stop`.

```bash
./main --socket /run/phantun-cp/agent.sock \
  --fail-phase pre_stop \
  --drop-phase post_stop
```

This is useful for shutdown-path testing.

## Multi-agent testing

Phantun can require multiple control agents by repeating `--control-target`.
Run one mock agent per socket path:

```bash
./main --socket /run/phantun-cp/agent-a.sock
./main --socket /run/phantun-cp/agent-b.sock
```

Then start Phantun with both targets:

```bash
phantun-server ... \
  --control-target /run/phantun-cp/agent-a.sock \
  --control-target /run/phantun-cp/agent-b.sock
```

Useful checks:

1. Both agents accept all phases: startup and shutdown succeed.
2. One agent rejects `pre_start`: startup fails.
3. One agent drops during runtime: quorum recovery path is exercised.
4. One agent rejects `pre_stop` or `post_stop`: shutdown logs barrier failure but
   still finishes within bounded time.

## Notes

1. This tool speaks the current `v2` request/response protocol. It does not use
   the older `ack`-style control-plane messages.
2. The mock agent logs every inbound JSON line and every outbound response, so it
   is suitable for manual protocol inspection during integration testing.
