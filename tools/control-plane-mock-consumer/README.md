# control-plane-mock-consumer

Minimal mock consumer for Phantun control-plane testing.

## Run

```bash
go run ./tools/control-plane-mock-consumer \
  --socket /tmp/phantun-cp/agent.sock
```

## Behavior

1. Listens on UNIX socket (server side).
2. Prints each incoming NDJSON line.
3. Sends `{"type":"ack","id":"..."}` when `data.state=="stopping"` (default).

## Options

```bash
go run ./tools/control-plane-mock-consumer --help
```

- `--socket`: UNIX socket listen path.
- `--ack-state`: Which state triggers ACK (default `stopping`).
- `--ack-type`: Optional message type filter for ACK (for example: `event`).
- `--ack-delay`: Delay ACK send (for timeout/path testing), for example: `200ms`.
- `--no-ack`: Disable ACK replies.
