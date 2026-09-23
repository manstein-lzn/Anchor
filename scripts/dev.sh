#!/usr/bin/env bash
set -euo pipefail

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
state="$repo/.local/dev"
mkdir -p "$state"

api_pid="$state/anchor-serve.pid"
web_pid="$state/vite.pid"

alive() {
  [[ -s "$1" ]] && kill -0 "$(<"$1")" 2>/dev/null
}

ready() {
  local url=$1 pid_file=$2 log=$3
  for _ in {1..50}; do
    curl -fsS --max-time 1 "$url" >/dev/null 2>&1 && return 0
    alive "$pid_file" || break
    sleep 0.1
  done
  echo "Failed to start $url; see $log" >&2
  return 1
}

start_api() {
  if curl -fsS --max-time 1 http://127.0.0.1:8077/graphs >/dev/null 2>&1; then
    echo "Anchor API already available at http://127.0.0.1:8077"
  elif alive "$api_pid"; then
    echo "Anchor API process already exists (pid $(<"$api_pid"))"
  else
    nohup setsid "$repo/.venv/bin/anchor-serve" \
      --root "$repo/.local/demo" --config "$repo/.local/runtime.json" \
      --host 127.0.0.1 --port 8077 \
      >"$state/anchor-serve.log" 2>&1 < /dev/null &
    echo "$!" > "$api_pid"
    ready http://127.0.0.1:8077/graphs "$api_pid" "$state/anchor-serve.log"
  fi
}

start_web() {
  if curl -fsS --max-time 1 http://127.0.0.1:5173/ >/dev/null 2>&1; then
    echo "Web UI already available at http://127.0.0.1:5173"
  elif alive "$web_pid"; then
    echo "Web UI process already exists (pid $(<"$web_pid"))"
  else
    nohup setsid "$repo/apps/web/node_modules/.bin/vite" \
      "$repo/apps/web" --host 127.0.0.1 --port 5173 \
      >"$state/vite.log" 2>&1 < /dev/null &
    echo "$!" > "$web_pid"
    ready http://127.0.0.1:5173/ "$web_pid" "$state/vite.log"
  fi
}

stop_one() {
  local label=$1 pid_file=$2
  if alive "$pid_file"; then
    local pid
    pid=$(<"$pid_file")
    kill -- "-$pid" 2>/dev/null || kill "$pid" 2>/dev/null || true
    for _ in {1..20}; do
      alive "$pid_file" || break
      sleep 0.1
    done
    echo "$label stopped"
  fi
  rm -f "$pid_file"
}

status() {
  curl -fsS --max-time 1 http://127.0.0.1:8077/graphs >/dev/null 2>&1 &&
    echo "Anchor API: up (http://127.0.0.1:8077)" || echo "Anchor API: down"
  curl -fsS --max-time 1 http://127.0.0.1:5173/ >/dev/null 2>&1 &&
    echo "Web UI:     up (http://127.0.0.1:5173)" || echo "Web UI:     down"
}

case "${1:-start}" in
  start)
    start_api
    start_web
    echo "Logs: $state/"
    ;;
  stop)
    stop_one "Web UI" "$web_pid"
    stop_one "Anchor API" "$api_pid"
    ;;
  restart)
    "$0" stop
    "$0" start
    ;;
  status)
    status
    ;;
  *)
    echo "usage: $0 [start|stop|restart|status]" >&2
    exit 2
    ;;
esac
