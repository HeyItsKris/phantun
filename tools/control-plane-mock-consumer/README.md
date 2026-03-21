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
