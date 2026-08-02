#!/usr/bin/env bash
# Serve a local llama.cpp model for AiTUI on an OpenAI-compatible endpoint.
#
# Usage:
#   scripts/llama-local.sh               # start the default model (qwen3-coder-30b)
#   scripts/llama-local.sh deepseek-r1-32b
#   scripts/llama-local.sh list
#   scripts/llama-local.sh stop
#
# Env overrides: LLAMA_MODEL_DIR (default ~/models), LLAMA_PORT (default 8080).
#
# AiTUI expects one model id per server instance. The loaded model id is the
# alias below; set it as `default_model` (and judge/suggestion/child/task model
# fields) in ~/.config/aitui/config.toml, or pick it in-app with Ctrl-M.
set -euo pipefail

MODEL_DIR="${LLAMA_MODEL_DIR:-$HOME/models}"
PORT="${LLAMA_PORT:-8080}"
THREADS="${LLAMA_THREADS:-14}"

models=(
  "qwen3-coder-30b:$MODEL_DIR/Qwen3-Coder-30B-A3B-Instruct-Q4_K_M.gguf:32768"
  "deepseek-r1-32b:$MODEL_DIR/DeepSeek-R1-Distill-Qwen-32B-Q4_K_M.gguf:16384"
)

list_models() {
  for entry in "${models[@]}"; do
    name="${entry%%:*}"
    rest="${entry#*:}"
    path="${rest%%:*}"
    ctx="${rest##*:}"
    if [ -f "$path" ]; then
      state="ok ($(du -h "$path" | cut -f1))"
    else
      state="missing"
    fi
    printf '  %-16s ctx %-6s %s\n' "$name" "$ctx" "$state"
  done
}

resolve() {
  for entry in "${models[@]}"; do
    name="${entry%%:*}"
    rest="${entry#*:}"
    path="${rest%%:*}"
    ctx="${rest##*:}"
    if [ "$name" = "$1" ]; then
      echo "$path|$ctx"
      return 0
    fi
  done
  return 1
}

case "${1:-}" in
  ""|qwen3-coder-30b|deepseek-r1-32b)
    name="${1:-qwen3-coder-30b}"
    ;;
  list|-l|--list)
    echo "Available models:"
    list_models
    exit 0
    ;;
  stop)
    if pgrep -f "llama-server.*--port $PORT" >/dev/null; then
      pkill -f "llama-server.*--port $PORT"
      echo "Stopped llama-server on port $PORT."
    else
      echo "No llama-server on port $PORT."
    fi
    exit 0
    ;;
  *)
    echo "Unknown model: $1" >&2
    echo "Available:" >&2
    list_models >&2
    exit 1
    ;;
esac

resolved="$(resolve "$name")"
path="${resolved%%|*}"
ctx="${resolved##*|}"
if [ ! -f "$path" ]; then
  echo "Model file missing: $path" >&2
  echo "Download it, e.g.:" >&2
  echo "  curl -L -o $path https://huggingface.co/unsloth/Qwen3-Coder-30B-A3B-Instruct-GGUF/resolve/main/Qwen3-Coder-30B-A3B-Instruct-Q4_K_M.gguf" >&2
  exit 1
fi

if pgrep -f "llama-server.*--port $PORT" >/dev/null; then
  echo "llama-server already running on port $PORT. Stop it first (scripts/llama-local.sh stop)." >&2
  exit 1
fi

# Give a just-killed server time to release the port so `stop && start` chains work.
for _ in $(seq 1 40); do
  if ! pgrep -f "llama-server.*--port $PORT" >/dev/null; then break; fi
  sleep 0.5
done

echo "Starting $name ($path, ctx $ctx) on http://127.0.0.1:$PORT …"
echo "Point AiTUI's endpoint at http://127.0.0.1:$PORT (model id: $name)."

exec llama-server \
  -m "$path" \
  --alias "$name" \
  -c "$ctx" \
  --host 127.0.0.1 \
  --port "$PORT" \
  --threads "$THREADS" \
  --no-webui
