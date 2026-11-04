#!/usr/bin/env bash
# =============================================================================
# Oqto All-in-One Entrypoint
# =============================================================================
#
# Starts all services in the correct dependency order:
#   2. eavs    (LLM proxy)
#   3. oqto    (backend API + WebSocket)
#   4. caddy   (reverse proxy, serves frontend)
#
# Environment variables:
#   Required:
#     At least one LLM provider key (ANTHROPIC_API_KEY, OPENAI_API_KEY, etc.)
#
#   Optional:
#     JWT_SECRET           - JWT signing secret (auto-generated + persisted if unset)
#     ADMIN_USER           - Bootstrap admin username (default: admin)
#     ADMIN_PASSWORD       - Bootstrap admin password (auto-generated if unset)
#     ADMIN_EMAIL          - Bootstrap admin email (default: admin@oqto.local)
#     OQTO_PORT            - External port (default: 8080)
#     EAVS_PORT            - Eavs internal port (default: 3033)
#     OQTO_LOG_LEVEL       - Log level: error/warn/info/debug/trace (default: info)
#     OQTO_DATA_DIR        - Data directory (default: /data)
#     OQTO_SINGLE_USER     - Single-user dev mode, bypass auth (default: false)
#
# =============================================================================

set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

OQTO_DATA_DIR="${OQTO_DATA_DIR:-/data}"
OQTO_PORT="${OQTO_PORT:-8080}"
EAVS_PORT="${EAVS_PORT:-3033}"
OQTO_LOG_LEVEL="${OQTO_LOG_LEVEL:-info}"
OQTO_SINGLE_USER="${OQTO_SINGLE_USER:-false}"
OQTO_CHAT_FIRST_RESPONSE_TIMEOUT_MS="${OQTO_CHAT_FIRST_RESPONSE_TIMEOUT_MS:-180000}"
OQTO_CHAT_STREAM_IDLE_TIMEOUT_MS="${OQTO_CHAT_STREAM_IDLE_TIMEOUT_MS:-60000}"

ADMIN_USER="${ADMIN_USER:-admin}"
ADMIN_EMAIL="${ADMIN_EMAIL:-admin@oqto.local}"
ADMIN_PASSWORD="${ADMIN_PASSWORD:-}"
JWT_SECRET="${JWT_SECRET:-}"

# Internal ports (not exposed)
OQTO_BACKEND_PORT=8081

PIDS=()

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

log() { echo "[oqto-init] $(date '+%H:%M:%S') $*"; }
log_error() { echo "[oqto-init] $(date '+%H:%M:%S') ERROR: $*" >&2; }

validate_timeout_ms() {
  local name="$1" value="$2" numeric
  case "$value" in
    ''|*[!0-9]*) log_error "${name} must be an integer from 1 to 2147483647"; return 1 ;;
  esac
  if [ "${#value}" -gt 10 ]; then
    log_error "${name} must be an integer from 1 to 2147483647"
    return 1
  fi
  numeric=$((10#$value))
  if [ "$numeric" -lt 1 ] || [ "$numeric" -gt 2147483647 ]; then
    log_error "${name} must be an integer from 1 to 2147483647"
    return 1
  fi
  return 0
}

validate_timeout_ms OQTO_CHAT_FIRST_RESPONSE_TIMEOUT_MS "$OQTO_CHAT_FIRST_RESPONSE_TIMEOUT_MS"
validate_timeout_ms OQTO_CHAT_STREAM_IDLE_TIMEOUT_MS "$OQTO_CHAT_STREAM_IDLE_TIMEOUT_MS"

generate_secret() {
  head -c 48 /dev/urandom | base64 | tr -d '/+=' | head -c "${1:-64}"
}

wait_for_port() {
  local port="$1" name="$2" timeout="${3:-30}" path="${4:-/health}"
  local waited=0
  while ! curl -sf "http://127.0.0.1:${port}${path}" >/dev/null 2>&1; do
    if [ "$waited" -ge "$timeout" ]; then
      log_error "${name} did not become healthy on port ${port}${path} within ${timeout}s"
      return 1
    fi
    sleep 1
    waited=$((waited + 1))
  done
  log "${name} is ready on port ${port} (${waited}s)"
}

wait_for_socket() {
  local socket_path="$1" name="$2" timeout="${3:-15}" waited=0
  while [ ! -S "$socket_path" ]; do
    if [ "$waited" -ge "$timeout" ]; then
      log_error "${name} socket not ready at ${socket_path} within ${timeout}s"
      return 1
    fi
    sleep 1
    waited=$((waited + 1))
  done
  log "${name} socket ready at ${socket_path} (${waited}s)"
}

cleanup() {
  log "Shutting down services..."
  for pid in "${PIDS[@]}"; do
    kill "$pid" 2>/dev/null || true
  done
  wait 2>/dev/null || true
  log "Shutdown complete."
  exit 0
}

trap cleanup SIGTERM SIGINT EXIT

# ---------------------------------------------------------------------------
# Directory setup
# ---------------------------------------------------------------------------

log "Initializing Oqto (data: ${OQTO_DATA_DIR})"

mkdir -p \
  "${OQTO_DATA_DIR}/eavs" \
  "${OQTO_DATA_DIR}/oqto" \
  "${OQTO_DATA_DIR}/users" \
  "${OQTO_DATA_DIR}/workspaces" \
  /run/oqto \
  /home/oqto/.config/oqto \
  /home/oqto/.config/eavs \
  "${OQTO_DATA_DIR}/pi-sessions"

chown -R oqto:oqto "${OQTO_DATA_DIR}" /run/oqto /home/oqto 2>/dev/null || true

# Symlink Pi sessions into the persistent volume so they survive container rebuilds
mkdir -p /home/oqto/.pi/agent
ln -sfn "${OQTO_DATA_DIR}/pi-sessions" /home/oqto/.pi/agent/sessions

# ---------------------------------------------------------------------------
# Generate secrets if not provided
# ---------------------------------------------------------------------------

if [ -z "$JWT_SECRET" ]; then
  JWT_SECRET_FILE="${OQTO_DATA_DIR}/oqto/.jwt_secret"
  if [ -f "$JWT_SECRET_FILE" ]; then
    JWT_SECRET=$(cat "$JWT_SECRET_FILE")
    log "Using persisted JWT secret"
  else
    JWT_SECRET=$(generate_secret 64)
    echo "$JWT_SECRET" > "$JWT_SECRET_FILE"
    chmod 600 "$JWT_SECRET_FILE"
    log "Generated and persisted JWT secret"
  fi
fi

EAVS_ADMIN_KEY_FILE="${OQTO_DATA_DIR}/eavs/.admin_key"
if [ -f "$EAVS_ADMIN_KEY_FILE" ]; then
  EAVS_ADMIN_KEY=$(cat "$EAVS_ADMIN_KEY_FILE")
else
  EAVS_ADMIN_KEY=$(generate_secret 32)
  echo "$EAVS_ADMIN_KEY" > "$EAVS_ADMIN_KEY_FILE"
  chmod 600 "$EAVS_ADMIN_KEY_FILE"
  log "Generated eavs admin key"
fi

# ---------------------------------------------------------------------------
# Write eavs config
# ---------------------------------------------------------------------------

EAVS_ENV_FILE="${OQTO_DATA_DIR}/eavs/eavs.env"

cat > "$EAVS_ENV_FILE" <<EOF
EAVS_PORT=${EAVS_PORT}
EAVS_ADMIN_KEY=${EAVS_ADMIN_KEY}
EOF

[ -n "${ANTHROPIC_API_KEY:-}" ]    && echo "ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY}" >> "$EAVS_ENV_FILE"
[ -n "${OPENAI_API_KEY:-}" ]       && echo "OPENAI_API_KEY=${OPENAI_API_KEY}" >> "$EAVS_ENV_FILE"
[ -n "${GEMINI_API_KEY:-}" ]       && echo "GEMINI_API_KEY=${GEMINI_API_KEY}" >> "$EAVS_ENV_FILE"
[ -n "${OPENROUTER_API_KEY:-}" ]   && echo "OPENROUTER_API_KEY=${OPENROUTER_API_KEY}" >> "$EAVS_ENV_FILE"
[ -n "${AZURE_API_KEY:-}" ]        && echo "AZURE_API_KEY=${AZURE_API_KEY}" >> "$EAVS_ENV_FILE"
[ -n "${DEEPSEEK_API_KEY:-}" ]     && echo "DEEPSEEK_API_KEY=${DEEPSEEK_API_KEY}" >> "$EAVS_ENV_FILE"
[ -n "${MISTRAL_API_KEY:-}" ]      && echo "MISTRAL_API_KEY=${MISTRAL_API_KEY}" >> "$EAVS_ENV_FILE"
[ -n "${FHGENIE_API_KEY:-}" ]      && echo "FHGENIE_API_KEY=${FHGENIE_API_KEY}" >> "$EAVS_ENV_FILE"

chmod 600 "$EAVS_ENV_FILE"

# Write eavs config.toml
EAVS_CONFIG_FILE="/home/oqto/.config/eavs/config.toml"
cat > "$EAVS_CONFIG_FILE" <<EOF
[server]
host = "127.0.0.1"
port = ${EAVS_PORT}

[admin]
api_key = "${EAVS_ADMIN_KEY}"

[keys]
enabled = true
database_path = "${OQTO_DATA_DIR}/eavs/keys.db"
master_key = "${EAVS_ADMIN_KEY}"
EOF

if [ -n "${ANTHROPIC_API_KEY:-}" ]; then
  cat >> "$EAVS_CONFIG_FILE" <<EOF

[providers.anthropic]
type = "anthropic"
api_key = "env:ANTHROPIC_API_KEY"
EOF
fi

if [ -n "${OPENAI_API_KEY:-}" ]; then
  cat >> "$EAVS_CONFIG_FILE" <<EOF

[providers.openai]
type = "openai"
api_key = "env:OPENAI_API_KEY"
EOF
fi

if [ -n "${GEMINI_API_KEY:-}" ]; then
  cat >> "$EAVS_CONFIG_FILE" <<EOF

[providers.google]
type = "google"
api_key = "env:GEMINI_API_KEY"
EOF
fi

if [ -n "${OPENROUTER_API_KEY:-}" ]; then
  cat >> "$EAVS_CONFIG_FILE" <<EOF

[providers.openrouter]
type = "openai"
api_key = "env:OPENROUTER_API_KEY"
base_url = "https://openrouter.ai/api/v1"
EOF
fi

if [ -n "${FHGENIE_API_KEY:-}" ]; then
  cat >> "$EAVS_CONFIG_FILE" <<EOF

[providers.fhgenie]
type = "openai-compatible"
api_key = "env:FHGENIE_API_KEY"
base_url = "${FHGENIE_BASE_URL:-https://fhgenie.fraunhofer.de/v1}"

[[providers.fhgenie.models]]
id = "${FHGENIE_MODEL:-nvidia/GLM-5.3-NVFP4}"
name = "GLM 5.3"
reasoning = true
EOF
fi

# Local LLM (OpenAI-compatible: Ollama, LM Studio, llama.cpp, vLLM, etc.)
if [ -n "${LOCAL_LLM_URL:-}" ]; then
  cat >> "$EAVS_CONFIG_FILE" <<EOF

[providers.local]
type = "openai-compatible"
base_url = "${LOCAL_LLM_URL}/v1"
EOF
  # Strip trailing /v1 if the user already included it
  sed -i 's|/v1/v1|/v1|g' "$EAVS_CONFIG_FILE"
fi

chown -R oqto:oqto /home/oqto/.config/eavs

# ---------------------------------------------------------------------------
# Write oqto config (matches AppConfig struct)
# ---------------------------------------------------------------------------

DEV_MODE="false"
LINUX_USERS_ENABLED="true"
RUNNER_SOCKET_PATTERN="/run/oqto/runner-sockets/{user}/oqto-runner.sock"
if [ "$OQTO_SINGLE_USER" = "true" ]; then
  DEV_MODE="true"
  LINUX_USERS_ENABLED="false"
  RUNNER_SOCKET_PATTERN="/run/oqto/oqto-runner.sock"
fi

cat > /home/oqto/.config/oqto/config.toml <<EOF
# Oqto Docker All-in-One configuration
# Generated by entrypoint.sh -- do not edit manually

profile = "docker"

[logging]
level = "${OQTO_LOG_LEVEL}"
format = "pretty"

[runtime]

[paths]
data_dir = "${OQTO_DATA_DIR}/oqto"

[backend]
mode = "local"

[local]
enabled = true
fileserver_binary = "oqto-files"
ttyd_binary = "ttyd"
workspace_dir = "${OQTO_DATA_DIR}/workspaces/{user_id}"
runner_socket_pattern = "${RUNNER_SOCKET_PATTERN}"
single_user = ${OQTO_SINGLE_USER}
cleanup_on_startup = true
stop_sessions_on_shutdown = true

[local.linux_users]
enabled = ${LINUX_USERS_ENABLED}
create_home = true
use_sudo = true

[auth]
dev_mode = ${DEV_MODE}
jwt_secret = "${JWT_SECRET}"
# The bundled Caddy proxies /api/* over loopback, so the backend peer is always
# 127.0.0.1. Trusting exactly that peer is what keeps the login rate limiter
# keyed per real client instead of per proxy.
trusted_proxies = ["127.0.0.1", "::1"]

[eavs]
enabled = true
base_url = "http://127.0.0.1:${EAVS_PORT}"
master_key = "${EAVS_ADMIN_KEY}"

[voice]
enabled = false

[sessions]

[mmry]
enabled = false

[sldr]

[server]
admin_socket_path = "/run/oqto/oqtoctl.sock"

[container]

[templates]

[scaffold]

[pi]

[agent_browser]
enabled = true
binary = "oqto-browser"
headed = false
stream_port_base = 30000
stream_port_range = 10000

[onboarding_templates]

[feedback]
EOF

chown -R oqto:oqto /home/oqto/.config/oqto
chmod 600 /home/oqto/.config/oqto/config.toml

# ---------------------------------------------------------------------------
# Write Caddyfile
# ---------------------------------------------------------------------------

cat > /etc/caddy/Caddyfile <<EOF
:${OQTO_PORT} {
    handle /health {
        respond "OK" 200
    }

    handle /runtime-config.js {
      root * /usr/local/share/oqto/frontend
      header Cache-Control "no-store"
      file_server
    }

    # API and WebSocket -> oqto backend
    handle /api/* {
        reverse_proxy 127.0.0.1:${OQTO_BACKEND_PORT}
    }

    handle /ws/* {
        reverse_proxy 127.0.0.1:${OQTO_BACKEND_PORT}
    }

    @websockets {
        header Connection *Upgrade*
        header Upgrade websocket
    }
    handle @websockets {
        reverse_proxy 127.0.0.1:${OQTO_BACKEND_PORT}
    }

    # Frontend static files
    handle {
        root * /usr/local/share/oqto/frontend
        try_files {path} /index.html
        file_server
    }

    # Local speech also requires HTTPS/loopback at the outer serving origin.
    header {
        Cross-Origin-Opener-Policy same-origin
        Cross-Origin-Embedder-Policy require-corp
        X-Content-Type-Options nosniff
        X-Frame-Options SAMEORIGIN
        Referrer-Policy strict-origin-when-cross-origin
        -Server
    }

    encode gzip zstd
}
EOF

# ---------------------------------------------------------------------------
# Start services
# ---------------------------------------------------------------------------

# 1. eavs (LLM proxy)
log "Starting eavs..."
set -a
# shellcheck source=/dev/null
source "$EAVS_ENV_FILE"
set +a
su -s /bin/bash oqto -c "
  export HOME=/home/oqto
  export XDG_CONFIG_HOME=/home/oqto/.config
  export XDG_DATA_HOME=${OQTO_DATA_DIR}
  $(env | grep -E '^(ANTHROPIC|OPENAI|GEMINI|OPENROUTER|AZURE|DEEPSEEK|MISTRAL|FHGENIE|EAVS_)' | sed 's/^/export /')
  eavs serve --port ${EAVS_PORT} --host 127.0.0.1 2>&1 | sed 's/^/[eavs] /'
" &
PIDS+=($!)
wait_for_port "$EAVS_PORT" "eavs" 15
EAVS_BASE_URL="http://127.0.0.1:${EAVS_PORT}"

# Pi discovers available models from its models.json, not directly from EAVS.
# Keep EAVS keys in the persistent volume so the embedded Pi key stays valid.
EAVS_PI_KEY_FILE="${OQTO_DATA_DIR}/eavs/.pi_api_key"
EAVS_PI_KEY_ID_FILE="${OQTO_DATA_DIR}/eavs/.pi_api_key_id"
EAVS_PI_KEY=""
if [ -s "$EAVS_PI_KEY_FILE" ] && [ -s "$EAVS_PI_KEY_ID_FILE" ]; then
  EAVS_PI_KEY_ID=$(cat "$EAVS_PI_KEY_ID_FILE")
  if curl -fsS -o /dev/null \
    -H "Authorization: Bearer ${EAVS_ADMIN_KEY}" \
    "${EAVS_BASE_URL}/admin/keys/${EAVS_PI_KEY_ID}"; then
    EAVS_PI_KEY=$(cat "$EAVS_PI_KEY_FILE")
  fi
fi

if [ -z "$EAVS_PI_KEY" ]; then
  log "Creating EAVS key for Pi..."
  EAVS_PI_KEY_RESPONSE=$(curl -fsS -X POST \
    -H "Authorization: Bearer ${EAVS_ADMIN_KEY}" \
    -H "Content-Type: application/json" \
    --data '{"name":"oqto-docker"}' \
    "${EAVS_BASE_URL}/admin/keys")
  EAVS_PI_KEY=$(printf '%s' "$EAVS_PI_KEY_RESPONSE" | jq -er '.key')
  EAVS_PI_KEY_ID=$(printf '%s' "$EAVS_PI_KEY_RESPONSE" | jq -er '.key_id')
  unset EAVS_PI_KEY_RESPONSE
  printf '%s\n' "$EAVS_PI_KEY" > "$EAVS_PI_KEY_FILE"
  printf '%s\n' "$EAVS_PI_KEY_ID" > "$EAVS_PI_KEY_ID_FILE"
  chmod 600 "$EAVS_PI_KEY_FILE"
  chmod 600 "$EAVS_PI_KEY_ID_FILE"
  chown oqto:oqto "$EAVS_PI_KEY_FILE" "$EAVS_PI_KEY_ID_FILE"
fi

PI_AGENT_DIR="/home/oqto/.pi/agent"
PI_MODELS_FILE="${PI_AGENT_DIR}/models.json"
PI_MODELS_TEMP="${PI_MODELS_FILE}.tmp"
mkdir -p "$PI_AGENT_DIR"
chown oqto:oqto "$PI_AGENT_DIR"

if ! curl -fsS "${EAVS_BASE_URL}/providers/detail" \
  | EAVS_PI_KEY="$EAVS_PI_KEY" jq --arg base_url "$EAVS_BASE_URL" '
    def pi_model:
      {
        id: .id,
        name: (if (.name // "") == "" then .id else .name end),
        reasoning: (.reasoning // false),
        input: ((.input // []) | if length == 0 then ["text"] else . end),
        contextWindow: (.context_window // 0),
        maxTokens: (.max_tokens // 0),
        cost: {
          input: (.cost.input // 0),
          output: (.cost.output // 0),
          cacheRead: (.cost.cache_read // 0),
          cacheWrite: (.cost.input // 0)
        }
      }
      + (if ((.compat // {}) | length) > 0 then {compat: .compat} else {} end);

    {
      providers: (
        [
          .[]
          | select(.name != "default" and (.pi_api | type) == "string")
          | {
              key: ("eavs-" + .name),
              value: (
                {
                  baseUrl: ($base_url + "/" + .name + "/v1"),
                  api: .pi_api,
                  apiKey: env.EAVS_PI_KEY,
                  models: [.models[]? | pi_model]
                }
                + (if ((.headers // {}) | length) > 0 then {headers: .headers} else {} end)
              )
            }
        ]
        | from_entries
      )
    }
  ' > "$PI_MODELS_TEMP"; then
  rm -f "$PI_MODELS_TEMP"
  log_error "Failed to generate Pi models.json from EAVS"
  exit 1
fi

mv "$PI_MODELS_TEMP" "$PI_MODELS_FILE"
chmod 600 "$PI_MODELS_FILE"
chown oqto:oqto "$PI_MODELS_FILE"

PI_MODEL_COUNT=$(jq '[.providers[]?.models[]?] | length' "$PI_MODELS_FILE")
if [ "$PI_MODEL_COUNT" -gt 0 ]; then
  PI_SETTINGS_FILE="${PI_AGENT_DIR}/settings.json"
  if [ ! -f "$PI_SETTINGS_FILE" ]; then
    PI_DEFAULT_SETTINGS=$(jq -c '
      [.providers | to_entries[]
        | select((.value.models // [] | length) > 0)
        | {defaultProvider: .key, defaultModel: .value.models[0].id}]
      | .[0] // empty
    ' "$PI_MODELS_FILE")
    if [ -n "$PI_DEFAULT_SETTINGS" ]; then
      printf '%s\n' "$PI_DEFAULT_SETTINGS" > "$PI_SETTINGS_FILE"
      chmod 600 "$PI_SETTINGS_FILE"
      chown oqto:oqto "$PI_SETTINGS_FILE"
    fi
  fi
  log "Pi model registry ready (${PI_MODEL_COUNT} model(s))"
else
  log_error "EAVS returned no Pi-compatible models"
fi

# Memory is embedded mmry-core (ADR-0010); no mmry daemon is started.

# 3. oqto-runner (single-user mode)
if [ "$OQTO_SINGLE_USER" = "true" ]; then
  log "Starting oqto-runner..."
  su -s /bin/bash oqto -c "
    export HOME=/home/oqto
    export XDG_RUNTIME_DIR=/run/oqto
    oqto-runner --socket /run/oqto/oqto-runner.sock --no-sandbox 2>&1 | sed 's/^/[runner] /'
  " &
  PIDS+=($!)
  wait_for_socket "/run/oqto/oqto-runner.sock" "oqto-runner" 20
  RUNNER_UID=$(id -u oqto)
  install -d -m 0700 -o oqto -g oqto "/run/user/${RUNNER_UID}"
  ln -sfn /run/oqto/oqto-runner.sock "/run/user/${RUNNER_UID}/oqto-runner.sock"
fi

# 5. oqto backend
log "Starting oqto backend..."

OQTO_LOG="/tmp/oqto.log"
su -s /bin/bash oqto -c "
  export HOME=/home/oqto
  export XDG_CONFIG_HOME=/home/oqto/.config
  export XDG_DATA_HOME=/home/oqto/.local/share
  export XDG_STATE_HOME=/home/oqto/.local/state
  export XDG_RUNTIME_DIR=/run/oqto
  export RUST_LOG=${OQTO_LOG_LEVEL:-info}
  oqto --config /home/oqto/.config/oqto/config.toml serve \
    --local-mode \
    --host 0.0.0.0 \
    --port ${OQTO_BACKEND_PORT} \
    --user-data-path ${OQTO_DATA_DIR}/users \
    >$OQTO_LOG 2>&1
" &
PIDS+=($!)

# Tail the log in background so we see output
tail -f "$OQTO_LOG" 2>/dev/null | sed 's/^/[oqto] /' &

# Wait for oqto, dump log on failure
if ! wait_for_port "$OQTO_BACKEND_PORT" "oqto" 30 "/api/health"; then
  log_error "oqto startup log:"
  cat "$OQTO_LOG" 2>/dev/null | sed 's/^/  /' || true
  exit 1
fi

# 6. Bootstrap admin user (first run only)
# Note: even in single-user/dev mode we create an admin account because the
# current frontend still uses the login screen.
if [ ! -f "${OQTO_DATA_DIR}/oqto/.bootstrapped" ]; then
  if [ -z "$ADMIN_PASSWORD" ]; then
    ADMIN_PASSWORD=$(generate_secret 16)
    log "============================================"
    log "  Admin credentials:"
    log "    Username: ${ADMIN_USER}"
    log "    Password: ${ADMIN_PASSWORD}"
    log "  Save these! They won't be shown again."
    log "============================================"
  fi

  log "Creating admin user..."
  su -s /bin/bash oqto -c "
    export HOME=/home/oqto
    export XDG_RUNTIME_DIR=/run/oqto
    oqtoctl user create \
      '${ADMIN_USER}' \
      -e '${ADMIN_EMAIL}' \
      -p '${ADMIN_PASSWORD}' \
      -r admin \
      2>&1
  " && log "Admin user created" \
    || log_error "Admin user creation failed"

  touch "${OQTO_DATA_DIR}/oqto/.bootstrapped"
fi

# 7. caddy (reverse proxy + frontend)
RUNTIME_CONFIG_FILE="/usr/local/share/oqto/frontend/runtime-config.js"
cat > "$RUNTIME_CONFIG_FILE" <<EOF
window.__OQTO_RUNTIME_CONFIG__ = {
  chatFirstResponseTimeoutMs: ${OQTO_CHAT_FIRST_RESPONSE_TIMEOUT_MS},
  chatStreamIdleTimeoutMs: ${OQTO_CHAT_STREAM_IDLE_TIMEOUT_MS}
};
EOF
chmod 644 "$RUNTIME_CONFIG_FILE"

log "Starting caddy..."
caddy run --config /etc/caddy/Caddyfile --adapter caddyfile 2>&1 | sed 's/^/[caddy] /' &
PIDS+=($!)

log "============================================"
log "  Oqto is running!"
log "  URL: http://localhost:${OQTO_HOST_PORT:-${OQTO_PORT}}"
log "============================================"

# Wait for any process to exit
wait -n "${PIDS[@]}" 2>/dev/null || true

# If any process exits, log which one and shut down
for i in "${!PIDS[@]}"; do
  if ! kill -0 "${PIDS[$i]}" 2>/dev/null; then
    log_error "Service (PID ${PIDS[$i]}) exited unexpectedly"
  fi
done

# Trigger cleanup
exit 1
