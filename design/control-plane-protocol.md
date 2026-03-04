# Control Plane Protocol (v1)

## 1. Overview

Phantun can publish runtime state updates to one or more UNIX domain socket targets.

- Transport: `AF_UNIX + SOCK_STREAM`
- Encoding: UTF-8
- Framing: NDJSON (`1 JSON object per line`, `\n` delimiter)
- Direction: **push fan-out** (Phantun is the connection initiator)

## 2. CLI

Use repeatable `--control-target`:

```bash
phantun-client ... \
  --control-target /run/phantun/agent-a.sock \
  --control-target /run/phantun/agent-b.sock
```

If no `--control-target` is passed, control-plane is disabled.

## 3. Connection behavior

For each target, Phantun runs an independent async sender task:

1. Try `connect()`.
2. On success, send one `snapshot`.
3. Send `event` for every state change.
4. If write fails or peer closes, reconnect with exponential backoff.

`snapshot` is always sent after reconnect so consumers can resync without replay logs.

## 4. Message schema

Every line is a JSON object:

```json
{
  "v": 1,
  "type": "snapshot",
  "ts": 1710000000000,
  "id": "m42",
  "data": {
    "state": "up",
    "reason": null,
    "mode": "client",
    "local": "127.0.0.1:1234",
    "remote": "1.2.3.4:4567",
    "dev": "tun0",
    "mtu": null,
    "addr4": "192.168.200.2/32",
    "addr6": "fcc8::2/128"
  }
}
```

Fields:

- `v`: protocol version (currently `1`)
- `type`: `snapshot | event`
- `ts`: Unix timestamp in milliseconds
- `id`: message id string (unique per sender task)
- `data`: state payload
  - `state`: `starting | up | down`
  - `reason`: `null | process_exit | main_loop_error | signal`
  - `mode`: `client | server`
  - `local`: local endpoint string when available
  - `remote`: remote endpoint string when available
  - `dev`: TUN device name when available
  - `mtu`: TUN MTU when available
  - `addr4`: IPv4 CIDR when available
  - `addr6`: IPv6 CIDR when available

## 5. State semantics

- `starting`: process is starting and control-plane is active.
- `up`: data-plane is initialized and ready (does **not** imply end-to-end peer reachability).
- `down`: process is shutting down or main loop failed.

`down.reason` values in v1:

- `process_exit`
- `main_loop_error`
- `signal`

`signal` is emitted when process receives termination signals (SIGINT/SIGTERM).

`up` state metadata (`dev`, `mtu`, `addr4`, `addr6`) is collected from kernel interface state
after TUN setup, not from raw CLI input values.

## 6. Consumer requirements

Consumer should:

1. Listen on its own UDS path before starting Phantun.
2. Parse NDJSON line-by-line.
3. Treat first message after each connection as authoritative `snapshot`.
4. Handle duplicate/late `event` safely (idempotent state machine).

## 7. Compatibility policy

- New fields may be added in future versions.
- Existing fields in v1 will not be removed or renamed in-place.
- Unknown fields must be ignored by consumers.
