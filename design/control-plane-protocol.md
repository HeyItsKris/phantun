# Control Plane Protocol (v2)

## 1. Overview

Phantun can synchronize its lifecycle with one or more local agents over UNIX domain sockets.

- Transport: `AF_UNIX + SOCK_STREAM`
- Encoding: UTF-8
- Framing: NDJSON (`1 JSON object per line`, `\n` delimiter)
- Direction: Phantun actively connects to every configured target
- Model: synchronous request/response over persistent connections

This protocol is not a best-effort event stream. It is a lifecycle coordination protocol.

## 2. CLI

Use repeatable `--control-target`:

```bash
phantun-client ... \
  --control-target /run/phantun/agent-a.sock \
  --control-target /run/phantun/agent-b.sock
```

Rules:

1. If no `--control-target` is passed, control-plane is disabled.
2. If one or more `--control-target` values are passed, every configured target becomes mandatory.
3. Startup fails if any configured agent cannot be connected within timeout.

## 3. Connection Behavior

For each configured target, Phantun maintains one long-lived outbound UDS connection.

Rules:

1. All configured targets must connect successfully before startup can continue.
2. Requests are sent over the persistent connection and each request expects one response.
3. If a connected agent disconnects after startup, Phantun enters bounded recovery.
4. During recovery, all missing agents must reconnect within timeout.
5. After reconnect, Phantun first sends a `sync_state` request, then re-runs `post_start`.
6. If recovery fails, Phantun requests controlled shutdown.

## 4. Request Phases

Only the following `phase` values are defined in `v2`:

1. `pre_start`
2. `post_start`
3. `sync_state`
4. `pre_stop`
5. `post_stop`

Semantics:

1. `pre_start`: startup gate; success is required before data-plane initialization.
2. `post_start`: post-init gate; success is required before stable `running`.
3. `sync_state`: reconnect-time state rehydration; does not advance lifecycle by itself.
4. `pre_stop`: shutdown gate; success is requested before data-plane stop begins.
5. `post_stop`: final cleanup confirmation after data-plane stop.

## 5. Message Schema

### 5.1 Request

Every request is a JSON object:

```json
{
  "v": 2,
  "kind": "request",
  "session_id": "boot-7f1f0e6b",
  "id": 12,
  "phase": "pre_start",
  "deadline_ms": 3000,
  "payload": {
    "state": "pre_start",
    "reason": null,
    "mode": "client",
    "local": "127.0.0.1:1234",
    "remote": "1.2.3.4:4567",
    "dev": null,
    "mtu": null,
    "addr4": null,
    "addr6": null
  }
}
```

### 5.2 Response

Every response is a JSON object:

```json
{
  "v": 2,
  "kind": "response",
  "session_id": "boot-7f1f0e6b",
  "id": 12,
  "success": true,
  "message": "accepted"
}
```

Failure example:

```json
{
  "v": 2,
  "kind": "response",
  "session_id": "boot-7f1f0e6b",
  "id": 12,
  "success": false,
  "message": "nft apply failed"
}
```

## 6. Fields

Top-level request fields:

- `v`: protocol version (`2`)
- `kind`: must be `request`
- `session_id`: identifies the current Phantun process instance
- `id`: per-session monotonically increasing request id
- `phase`: lifecycle request phase
- `deadline_ms`: agent-side handling deadline hint
- `payload`: current runtime context

Top-level response fields:

- `v`: protocol version (`2`)
- `kind`: must be `response`
- `session_id`: must match the request
- `id`: must match the request
- `success`: `true | false`
- `message`: optional text for logs and diagnostics

`payload` fields:

- `state`: current lifecycle state
- `reason`: optional stop reason
- `mode`: `client | server`
- `local`: local endpoint when available
- `remote`: remote endpoint when available
- `dev`: TUN device name when available
- `mtu`: TUN MTU when available
- `addr4`: IPv4 CIDR when available
- `addr6`: IPv6 CIDR when available

## 7. Lifecycle States

`payload.state` can be:

1. `pre_start`
2. `starting`
3. `post_start`
4. `running`
5. `pre_stop`
6. `stopping`
7. `post_stop`
8. `stopped`

Rules:

1. Lifecycle is monotonic; states must not move backward within one `session_id`.
2. If an agent receives a snapshot-like `sync_state` request with state `running`, it should treat earlier startup phases as already completed.
3. Agents must not replay or re-run earlier phases after seeing a later state.

## 8. Required Fields By Phase

### `pre_start`

Required:

- `state`
- `mode`
- `local`
- `remote`

Optional:

- `dev`
- `mtu`
- `addr4`
- `addr6`
- `reason`

Additional rules:

1. `state` must be `pre_start`.
2. Interface fields may be empty because data-plane init has not happened yet.

### `post_start`

Required:

- `state`
- `mode`
- `local`
- `remote`
- `dev`
- `mtu`

Conditionally required:

- `addr4` if IPv4 is enabled
- `addr6` if IPv6 is enabled

Optional:

- `reason`

Additional rules:

1. `state` must be `post_start` or `running`.
2. Receiving `post_start` implies earlier startup phases already succeeded.

### `sync_state`

Rules:

1. `sync_state` itself adds no new payload field.
2. Requiredness is determined by the current `payload.state`.
3. If `state` is `running`, validate payload like `post_start`.
4. If `state` is `pre_stop` or `stopping`, `reason` must be present.
5. If `state` is `post_stop` or `stopped`, interface fields may be empty.

### `pre_stop`

Required:

- `state`
- `mode`
- `reason`

Recommended:

- `local`
- `remote`
- `dev`
- `mtu`
- `addr4`
- `addr6`

Additional rules:

1. `state` must be `pre_stop`.
2. `reason` must not be empty.

`reason` values in `v2`:

- `process_exit`
- `main_loop_error`
- `signal`
- `agent_disconnect`

### `post_stop`

Required:

- `state`
- `mode`

Optional:

- `local`
- `remote`
- `dev`
- `mtu`
- `addr4`
- `addr6`
- `reason`

Additional rules:

1. `state` must be `post_stop` or `stopped`.
2. Interface fields are allowed to be empty because data-plane is already down.

## 9. Agent Requirements

Each agent should:

1. listen on its own UDS path before starting Phantun
2. keep the connection open after successful `post_start`
3. parse NDJSON line-by-line
4. validate required fields for the given `phase`
5. return one response per request
6. preserve `session_id` and `id` exactly
7. return `success=false` when required fields are missing or phase handling fails

Agents are active lifecycle participants, not passive observers.

## 10. Failure Semantics

The following are phase failures:

1. agent connect timeout
2. disconnect while waiting for a phase response
3. malformed response
4. wrong `session_id`
5. wrong `id`
6. `success=false`
7. response timeout

Default handling:

1. startup failure aborts startup
2. runtime disconnect enters bounded recovery
3. failed recovery requests controlled shutdown

## 11. Compatibility

`v2` is intentionally incompatible with the older async event-stream design.

Compatibility rules:

1. `v2` responses must ignore unknown request fields.
2. Existing `v2` fields must not be renamed in-place.
3. New optional fields may be added in future versions.
