#!/bin/sh
set -eu

# Linux nftables template.
# This follows Phantun's documented NAT model:
# - client: srcnat/masquerade the tunnel peer address on the uplink
# - server: dstnat the listening TCP port to the tunnel peer address
# - pre_stop: remove NAT entry rules so new flows stop entering
# - post_stop: remove the remaining forward rules
#
# This template does not create its own forward base chain. Instead, it adds
# rules into an existing filter/nat ruleset and removes them again by comment.

: "${PHANTUN_PROTOCOL_VERSION:?missing PHANTUN_PROTOCOL_VERSION}"
: "${PHANTUN_KIND:?missing PHANTUN_KIND}"
: "${PHANTUN_SESSION_ID:?missing PHANTUN_SESSION_ID}"
: "${PHANTUN_REQUEST_ID:?missing PHANTUN_REQUEST_ID}"
: "${PHANTUN_PHASE:?missing PHANTUN_PHASE}"
: "${PHANTUN_DEADLINE_MS:?missing PHANTUN_DEADLINE_MS}"
: "${PHANTUN_STATE:?missing PHANTUN_STATE}"
: "${PHANTUN_MODE:?missing PHANTUN_MODE}"

NFT="${NFT_BIN:-nft}"
NFT_FILTER_FAMILY="${NFT_FILTER_FAMILY:-inet}"
NFT_FILTER_TABLE="${NFT_FILTER_TABLE:-filter}"
NFT_FORWARD_CHAIN="${NFT_FORWARD_CHAIN:-forward}"
NFT_NAT_FAMILY="${NFT_NAT_FAMILY:-inet}"
NFT_NAT_TABLE="${NFT_NAT_TABLE:-nat}"
NFT_PREROUTING_CHAIN="${NFT_PREROUTING_CHAIN:-prerouting}"
NFT_POSTROUTING_CHAIN="${NFT_POSTROUTING_CHAIN:-postrouting}"

SESSION_ID="${PHANTUN_SESSION_ID}"
REQUEST_ID="${PHANTUN_REQUEST_ID}"
PHASE="${PHANTUN_PHASE}"
STATE="${PHANTUN_STATE}"
MODE="${PHANTUN_MODE}"
REASON="${PHANTUN_REASON:-}"
LOCAL="${PHANTUN_LOCAL:-}"
REMOTE="${PHANTUN_REMOTE:-}"
DEV="${PHANTUN_DEV:-}"
MTU="${PHANTUN_MTU:-}"
ADDR4="${PHANTUN_ADDR4:-}"
ADDR6="${PHANTUN_ADDR6:-}"
PEER4="${PHANTUN_PEER4:-}"
PEER6="${PHANTUN_PEER6:-}"

log() {
  printf '[phantun-agent][nftables] %s\n' "$*"
}

fail() {
  printf '[phantun-agent][nftables] %s\n' "$*" >&2
  exit 1
}

run() {
  log "+ $*"
  "$@"
}

tool_exists() {
  command -v "$1" >/dev/null 2>&1
}

sanitize_id() {
  value=$(printf '%s' "$1" | tr '[:upper:]/:-.' '[:lower:]_____' | tr -cd 'a-z0-9_' | cut -c1-24)
  [ -n "$value" ] || value="default"
  printf '%s' "$value"
}

cidr_addr() {
  printf '%s' "$1" | awk -F/ '{print $1}'
}

default_iface_v4() {
  ip -4 route show default 2>/dev/null | awk '{for (i = 1; i <= NF; i++) if ($i == "dev") {print $(i + 1); exit}}'
}

default_iface_v6() {
  ip -6 route show default 2>/dev/null | awk '{for (i = 1; i <= NF; i++) if ($i == "dev") {print $(i + 1); exit}}'
}

parse_port() {
  value="$1"
  if [ -z "$value" ]; then
    return 1
  fi
  printf '%s' "${value##*:}"
}

RULE_TAG="phantun_$(sanitize_id "${MODE}_${DEV:-${SESSION_ID}}_${LOCAL:-na}")"

dump_context() {
  log "session_id=${SESSION_ID} request_id=${REQUEST_ID} phase=${PHASE} state=${STATE} mode=${MODE}"
  log "local=${LOCAL} remote=${REMOTE} dev=${DEV} mtu=${MTU} addr4=${ADDR4} addr6=${ADDR6} peer4=${PEER4} peer6=${PEER6} reason=${REASON}"
  log "rule_tag=${RULE_TAG}"
  log "filter=${NFT_FILTER_FAMILY}/${NFT_FILTER_TABLE}/${NFT_FORWARD_CHAIN} nat=${NFT_NAT_FAMILY}/${NFT_NAT_TABLE}/${NFT_PREROUTING_CHAIN}:${NFT_POSTROUTING_CHAIN}"
}

require_any_peer() {
  if [ -z "$PEER4" ] && [ -z "$PEER6" ]; then
    fail "requires PHANTUN_PEER4 or PHANTUN_PEER6"
  fi
}

chain_exists() {
  family="$1"
  table="$2"
  chain="$3"
  "$NFT" list chain "$family" "$table" "$chain" >/dev/null 2>&1
}

require_filter_chain() {
  chain_exists "$NFT_FILTER_FAMILY" "$NFT_FILTER_TABLE" "$NFT_FORWARD_CHAIN" || \
    fail "missing nftables forward chain: ${NFT_FILTER_FAMILY}/${NFT_FILTER_TABLE}/${NFT_FORWARD_CHAIN}"
}

require_postrouting_chain() {
  chain_exists "$NFT_NAT_FAMILY" "$NFT_NAT_TABLE" "$NFT_POSTROUTING_CHAIN" || \
    fail "missing nftables postrouting chain: ${NFT_NAT_FAMILY}/${NFT_NAT_TABLE}/${NFT_POSTROUTING_CHAIN}"
}

require_prerouting_chain() {
  chain_exists "$NFT_NAT_FAMILY" "$NFT_NAT_TABLE" "$NFT_PREROUTING_CHAIN" || \
    fail "missing nftables prerouting chain: ${NFT_NAT_FAMILY}/${NFT_NAT_TABLE}/${NFT_PREROUTING_CHAIN}"
}

rule_handles_by_comment() {
  family="$1"
  table="$2"
  chain="$3"

  "$NFT" -a list chain "$family" "$table" "$chain" 2>/dev/null | awk -v tag="$RULE_TAG" '
    index($0, "comment \"" tag "\"") {
      for (i = 1; i <= NF; i++) {
        if ($i == "handle") {
          print $(i + 1)
        }
      }
    }
  '
}

delete_rules_by_comment() {
  family="$1"
  table="$2"
  chain="$3"

  if ! chain_exists "$family" "$table" "$chain"; then
    return 0
  fi

  rule_handles_by_comment "$family" "$table" "$chain" | sort -rn | while IFS= read -r handle; do
    [ -n "$handle" ] || continue
    run "$NFT" delete rule "$family" "$table" "$chain" handle "$handle"
  done
}

ensure_forward_rules() {
  require_filter_chain
  delete_rules_by_comment "$NFT_FILTER_FAMILY" "$NFT_FILTER_TABLE" "$NFT_FORWARD_CHAIN"
  run "$NFT" add rule "$NFT_FILTER_FAMILY" "$NFT_FILTER_TABLE" "$NFT_FORWARD_CHAIN" iifname "$DEV" counter accept comment "$RULE_TAG"
  run "$NFT" add rule "$NFT_FILTER_FAMILY" "$NFT_FILTER_TABLE" "$NFT_FORWARD_CHAIN" oifname "$DEV" counter accept comment "$RULE_TAG"
}

delete_forward_rules() {
  delete_rules_by_comment "$NFT_FILTER_FAMILY" "$NFT_FILTER_TABLE" "$NFT_FORWARD_CHAIN"
}

apply_client_nat() {
  require_postrouting_chain

  delete_rules_by_comment "$NFT_NAT_FAMILY" "$NFT_NAT_TABLE" "$NFT_POSTROUTING_CHAIN"

  if [ -n "$PEER4" ]; then
    iface4="$(default_iface_v4)"
    [ -n "$iface4" ] || fail "client IPv4 requires a default uplink interface"
    addr4="$(cidr_addr "$PEER4")"
    run "$NFT" add rule "$NFT_NAT_FAMILY" "$NFT_NAT_TABLE" "$NFT_POSTROUTING_CHAIN" \
      ip saddr "$addr4" oifname "$iface4" counter masquerade comment "$RULE_TAG"
  fi

  if [ -n "$PEER6" ]; then
    iface6="$(default_iface_v6)"
    [ -n "$iface6" ] || fail "client IPv6 requires a default uplink interface"
    addr6="$(cidr_addr "$PEER6")"
    run "$NFT" add rule "$NFT_NAT_FAMILY" "$NFT_NAT_TABLE" "$NFT_POSTROUTING_CHAIN" \
      ip6 saddr "$addr6" oifname "$iface6" counter masquerade comment "$RULE_TAG"
  fi
}

apply_server_nat() {
  require_prerouting_chain

  delete_rules_by_comment "$NFT_NAT_FAMILY" "$NFT_NAT_TABLE" "$NFT_PREROUTING_CHAIN"

  port="$(parse_port "$LOCAL")"
  [ -n "$port" ] || fail "server mode requires a local listen port"

  if [ -n "$PEER4" ]; then
    iface4="$(default_iface_v4)"
    [ -n "$iface4" ] || fail "server IPv4 requires a default uplink interface"
    addr4="$(cidr_addr "$PEER4")"
    run "$NFT" add rule "$NFT_NAT_FAMILY" "$NFT_NAT_TABLE" "$NFT_PREROUTING_CHAIN" \
      iifname "$iface4" tcp dport "$port" counter dnat ip to "$addr4" comment "$RULE_TAG"
  fi

  if [ -n "$PEER6" ]; then
    iface6="$(default_iface_v6)"
    [ -n "$iface6" ] || fail "server IPv6 requires a default uplink interface"
    addr6="$(cidr_addr "$PEER6")"
    run "$NFT" add rule "$NFT_NAT_FAMILY" "$NFT_NAT_TABLE" "$NFT_PREROUTING_CHAIN" \
      iifname "$iface6" tcp dport "$port" counter dnat ip6 to "$addr6" comment "$RULE_TAG"
  fi
}

apply_rules() {
  [ -n "$DEV" ] || fail "post_start requires PHANTUN_DEV"
  require_any_peer
  ensure_forward_rules

  case "$MODE" in
    client)
      apply_client_nat
      ;;
    server)
      apply_server_nat
      ;;
    *)
      fail "unsupported PHANTUN_MODE: ${MODE}"
      ;;
  esac
}

quiesce_rules() {
  case "$MODE" in
    client)
      delete_rules_by_comment "$NFT_NAT_FAMILY" "$NFT_NAT_TABLE" "$NFT_POSTROUTING_CHAIN"
      ;;
    server)
      delete_rules_by_comment "$NFT_NAT_FAMILY" "$NFT_NAT_TABLE" "$NFT_PREROUTING_CHAIN"
      ;;
    *)
      fail "unsupported PHANTUN_MODE: ${MODE}"
      ;;
  esac
}

cleanup_rules() {
  quiesce_rules
  delete_forward_rules
}

pre_start() {
  log "pre_start"
  dump_context

  tool_exists "$NFT" || fail "nft not found: ${NFT}"
  tool_exists ip || fail "ip not found"
  require_filter_chain

  case "$MODE" in
    client)
      require_postrouting_chain
      ;;
    server)
      require_prerouting_chain
      ;;
    *)
      fail "unsupported PHANTUN_MODE: ${MODE}"
      ;;
  esac

  # TODO:
  # Add any pre-flight checks here, such as verifying sysctl forwarding state.
  :
}

post_start() {
  log "post_start"
  dump_context
  apply_rules
}

sync_state() {
  log "sync_state"
  dump_context

  case "$STATE" in
    post_start|running)
      apply_rules
      ;;
    pre_stop|stopping)
      quiesce_rules
      ;;
    post_stop|stopped)
      cleanup_rules
      ;;
    *)
      log "sync_state no-op for state=${STATE}"
      ;;
  esac
}

pre_stop() {
  log "pre_stop"
  dump_context
  quiesce_rules
}

post_stop() {
  log "post_stop"
  dump_context
  cleanup_rules
}

main() {
  case "$PHASE" in
    pre_start)
      pre_start
      ;;
    post_start)
      post_start
      ;;
    sync_state)
      sync_state
      ;;
    pre_stop)
      pre_stop
      ;;
    post_stop)
      post_stop
      ;;
    *)
      fail "unknown phase: ${PHASE}"
      ;;
  esac
}

main "$@"
