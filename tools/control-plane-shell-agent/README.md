# control-plane-shell-agent

Minimal Linux control-plane agent for Phantun `v2`.

It listens on a UNIX domain socket, receives Phantun control-plane `request`
messages, exports request data as environment variables, executes one fixed `sh`
script, and returns a `response` based on the script result.

## Behavior

1. Transport: `AF_UNIX + SOCK_STREAM`
2. Framing: NDJSON (`1 JSON object per line`)
3. Protocol: Phantun control-plane `v2`
4. Script execution: `/bin/sh <script>`
5. Request context: environment variables only
6. Success rule: script exit code `0`
7. Failure rule: non-zero exit code, timeout, or script start error
8. Concurrency: script execution is globally serialized inside the agent

The agent does not pass lifecycle phase or payload as command-line arguments.
The script reads everything from environment variables.

## Build

```bash
go build -o phantun-agent ./tools/control-plane-shell-agent
```

## Run

```bash
./phantun-agent \
  --socket /run/phantun/agent.sock \
  --script /opt/phantun-agent/handler.sh
```

Options:

- `--socket`: UNIX socket path to listen on
- `--script`: shell script path to execute for every request
- `--max-script-timeout`: hard upper bound for script execution, default `10s`

## Phantun side

Point Phantun at the same socket path:

```bash
phantun-server ... --control-target /run/phantun/agent.sock
```

or:

```bash
phantun-client ... --control-target /run/phantun/agent.sock
```

## Environment Variables

The agent clears any inherited `PHANTUN_*` variables and then exports the
current request context:

- `PHANTUN_PROTOCOL_VERSION`
- `PHANTUN_KIND`
- `PHANTUN_SESSION_ID`
- `PHANTUN_REQUEST_ID`
- `PHANTUN_PHASE`
- `PHANTUN_DEADLINE_MS`
- `PHANTUN_STATE`
- `PHANTUN_MODE`
- `PHANTUN_REASON`
- `PHANTUN_LOCAL`
- `PHANTUN_REMOTE`
- `PHANTUN_DEV`
- `PHANTUN_MTU`
- `PHANTUN_ADDR4`
- `PHANTUN_ADDR6`
- `PHANTUN_PEER4`
- `PHANTUN_PEER6`

Optional fields are only exported when present in the request payload.

The bundled agent itself validates only the protocol envelope and base required
fields:

- `v`
- `kind`
- `session_id`
- `phase`
- `payload.state`
- `payload.mode`

Phase-specific payload validation is intentionally delegated to the script. The
bundled firewall templates already fail on missing fields such as:

- `PHANTUN_DEV`
- `PHANTUN_PEER4`
- `PHANTUN_PEER6`
- `PHANTUN_LOCAL` in server mode

Field split:

- `PHANTUN_ADDR4` / `PHANTUN_ADDR6`: kernel-reported local interface addresses
- `PHANTUN_PEER4` / `PHANTUN_PEER6`: kernel-reported point-to-point peer/destination addresses

The bundled NAT templates follow Phantun's official semantics and only use:

- `PHANTUN_PEER4`
- `PHANTUN_PEER6`

`PHANTUN_ADDR4` / `PHANTUN_ADDR6` are still exported, but the templates keep
them only for diagnostics instead of NAT rule generation.

## Script Contract

The agent always runs the same script:

```bash
/bin/sh /opt/phantun-agent/handler.sh
```

No extra command-line arguments are passed.

The script should:

1. Read phase and payload from environment variables
2. Exit `0` on success
3. Exit non-zero on failure
4. Write useful failure details to `stderr`

A ready-to-edit demo template is included at:

```bash
tools/control-plane-shell-agent/handler.template.sh
```

There are also three firewall-oriented variants:

```bash
tools/control-plane-shell-agent/handler.iptables.template.sh
tools/control-plane-shell-agent/handler.nftables.template.sh
tools/control-plane-shell-agent/handler.openwrt.template.sh
```

Use them like this:

- regular Linux + `iptables`/`ip6tables`: `handler.iptables.template.sh`
- regular Linux + `nftables`: `handler.nftables.template.sh`
- OpenWrt `firewall4` (modern OpenWrt, nftables-based): `handler.openwrt.template.sh`

If you are on legacy OpenWrt `firewall3`/`iptables`, start from the iptables
template instead.

These three variants follow Phantun's official NAT semantics rather than a
"forward-only" model:

- `client`: `SNAT/MASQUERADE` the TUN address on the uplink
- `server`: `DNAT` the listening TCP port to the TUN address
- `pre_stop`: remove NAT entry rules first so no new flows enter
- `post_stop`: remove the remaining forwarding state and helper objects

It already normalizes the request into shell variables and splits the lifecycle
into phase-specific functions. It also includes:

- `log`: regular log output
- `fail`: error output with exit
- `run`: a single command wrapper for future `nft` / `iptables` calls
- `dump_context`: current request context for debugging

In practice, you only need to replace the `TODO` blocks in each phase with your
real firewall commands.

Template-specific notes:

- `handler.iptables.template.sh`: uses dedicated custom chains, so cleanup does
  not depend on the current default route and does not delete unrelated host
  rules.
- `handler.nftables.template.sh`: adds rules into an existing nftables ruleset
  and removes them by comment tag. By default it expects:
  - filter chain: `inet/filter/forward`
  - nat chains: `inet/nat/prerouting` and `inet/nat/postrouting`
  You can override these with:
  - `NFT_FILTER_FAMILY`
  - `NFT_FILTER_TABLE`
  - `NFT_FORWARD_CHAIN`
  - `NFT_NAT_FAMILY`
  - `NFT_NAT_TABLE`
  - `NFT_PREROUTING_CHAIN`
  - `NFT_POSTROUTING_CHAIN`
- `handler.openwrt.template.sh`: writes an include file for `fw4` and appends
  rules into `inet fw4 forward`, `inet fw4 srcnat`, and `inet fw4 dstnat`.

Example:

```sh
#!/bin/sh
set -eu

PHASE="${PHANTUN_PHASE:?missing PHANTUN_PHASE}"
DEV="${PHANTUN_DEV:-}"

run() {
  printf '+ %s\n' "$*"
  "$@"
}

case "${PHASE}" in
  pre_start)
    :
    ;;
  post_start)
    [ -n "${DEV}" ] || {
      echo "missing PHANTUN_DEV" >&2
      exit 1
    }
    # run nft add rule inet phantun input iifname "${DEV}" accept
    ;;
  sync_state)
    :
    ;;
  pre_stop)
    :
    ;;
  post_stop)
    :
    ;;
  *)
    echo "unknown phase: ${PHASE}" >&2
    exit 2
    ;;
esac
```

## Timeout Rules

For each request, the effective script timeout is:

```text
min(request.deadline_ms, --max-script-timeout)
```

If `deadline_ms` is missing or `0`, the agent uses only
`--max-script-timeout`.

On timeout:

1. The agent kills the whole script process group
2. The agent returns `success=false`
3. The response message becomes `script timed out after ...`

If the agent itself is shutting down and cancels the script context first, the
response message becomes:

```text
script canceled by agent shutdown
```

## Response Rules

When the script exits:

- exit `0` -> `success=true`
- non-zero -> `success=false`

Failure message priority:

1. `stderr`
2. `stdout`
3. synthetic message such as `script exited with code 1`

Messages are truncated before they are placed in the response.

## Notes

1. The agent is Linux-oriented and uses UNIX socket and process-group kill
   semantics.
2. Because script execution is globally serialized, multiple concurrent Phantun
   instances using one agent will run their hooks one at a time.
3. The agent is intentionally minimal. It does not implement plugin loading,
   retries, or an internal hook framework.
