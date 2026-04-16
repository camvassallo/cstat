#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
LOG_DIR="$ROOT/logs"
mkdir -p "$LOG_DIR"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
NC='\033[0m'

usage() {
  echo "Usage: $0 {start|stop|restart|logs|status}"
  echo ""
  echo "Commands:"
  echo "  start       Start Postgres, API server, and web frontend"
  echo "  stop        Stop all running services"
  echo "  restart     Stop then start all services"
  echo "  logs [svc]  Tail logs (all, or: api, web, postgres)"
  echo "  status      Show status of each service"
  exit 1
}

pid_file() { echo "$LOG_DIR/$1.pid"; }

is_running() {
  local pf
  pf="$(pid_file "$1")"
  [[ -f "$pf" ]] && kill -0 "$(cat "$pf")" 2>/dev/null
}

kill_port() {
  local port=$1
  local pids
  pids="$(lsof -ti :"$port" 2>/dev/null || true)"
  if [[ -n "$pids" ]]; then
    echo -e "${YELLOW}Killing existing process(es) on port $port...${NC}"
    echo "$pids" | xargs kill 2>/dev/null || true
    sleep 1
  fi
}

start_postgres() {
  echo -e "${CYAN}Starting Postgres...${NC}"
  docker compose -f "$ROOT/docker-compose.yml" up -d 2>&1 | tee "$LOG_DIR/postgres.log"

  # Wait for Postgres to accept connections
  local tries=0
  while ! docker exec cstat-postgres pg_isready -U cstat >/dev/null 2>&1; do
    tries=$((tries + 1))
    if [[ $tries -ge 30 ]]; then
      echo -e "${RED}Postgres failed to become ready${NC}"
      exit 1
    fi
    sleep 1
  done
  echo -e "${GREEN}Postgres is ready${NC}"
}

start_api() {
  if is_running api; then
    echo -e "${YELLOW}API server already running (pid $(cat "$(pid_file api)"))${NC}"
    return
  fi

  kill_port 8080
  echo -e "${CYAN}Building & starting API server...${NC}"
  # Source .env if present
  if [[ -f "$ROOT/.env" ]]; then
    set -a; source "$ROOT/.env"; set +a
  fi

  cargo run -p cstat-api --manifest-path "$ROOT/Cargo.toml" \
    >"$LOG_DIR/api.log" 2>&1 &
  local pid=$!
  echo "$pid" > "$(pid_file api)"
  echo -e "${GREEN}API server started (pid $pid) — logs: logs/api.log${NC}"
}

start_web() {
  if is_running web; then
    echo -e "${YELLOW}Web frontend already running (pid $(cat "$(pid_file web)"))${NC}"
    return
  fi

  kill_port 5173
  echo -e "${CYAN}Starting web frontend...${NC}"
  cd "$ROOT/web"
  npm run dev >"$LOG_DIR/web.log" 2>&1 &
  local pid=$!
  echo "$pid" > "$(pid_file web)"
  cd "$ROOT"
  echo -e "${GREEN}Web frontend started (pid $pid) — logs: logs/web.log${NC}"
}

do_start() {
  start_postgres
  start_api
  start_web
  echo ""
  echo -e "${GREEN}All services started!${NC}"
  echo -e "  Frontend: ${CYAN}http://localhost:5173${NC}"
  echo -e "  API:      ${CYAN}http://localhost:8080${NC}"
  echo -e "  Postgres: ${CYAN}localhost:5432${NC}"
  echo ""
  echo "Run '$0 logs' to tail all logs, or '$0 logs api' for a specific service."
}

do_stop() {
  for svc in api web; do
    if is_running "$svc"; then
      local pid
      pid="$(cat "$(pid_file "$svc")")"
      echo -e "${YELLOW}Stopping $svc (pid $pid)...${NC}"
      kill "$pid" 2>/dev/null || true
      rm -f "$(pid_file "$svc")"
    else
      echo -e "$svc not running"
    fi
  done

  echo -e "${YELLOW}Stopping Postgres...${NC}"
  docker compose -f "$ROOT/docker-compose.yml" down 2>&1
  echo -e "${GREEN}All services stopped${NC}"
}

do_status() {
  # Postgres
  if docker exec cstat-postgres pg_isready -U cstat >/dev/null 2>&1; then
    echo -e "  postgres: ${GREEN}running${NC}"
  else
    echo -e "  postgres: ${RED}stopped${NC}"
  fi

  # API & Web
  for svc in api web; do
    if is_running "$svc"; then
      echo -e "  $svc: ${GREEN}running${NC} (pid $(cat "$(pid_file "$svc")"))"
    else
      echo -e "  $svc: ${RED}stopped${NC}"
    fi
  done
}

do_logs() {
  local svc="${1:-all}"
  case "$svc" in
    all)      tail -f "$LOG_DIR/api.log" "$LOG_DIR/web.log" ;;
    api)      tail -f "$LOG_DIR/api.log" ;;
    web)      tail -f "$LOG_DIR/web.log" ;;
    postgres) docker compose -f "$ROOT/docker-compose.yml" logs -f ;;
    *)        echo "Unknown service: $svc"; exit 1 ;;
  esac
}

[[ $# -lt 1 ]] && usage

case "$1" in
  start)   do_start ;;
  stop)    do_stop ;;
  restart) do_stop; do_start ;;
  status)  do_status ;;
  logs)    do_logs "${2:-all}" ;;
  *)       usage ;;
esac
