#!/usr/bin/env bash
# One-command local run for macOS:  ./scripts/dev-mac.sh  [--no-db] [--iceberg]
#
# 1. Checks Xcode Command Line Tools, installs Rust (rustup) if missing, picks Node 24 via nvm.
# 2. npm install, fetches the Amazon RDS CA bundle (optional).
# 3. Starts seeded Postgres + Valkey in Docker (Rancher Desktop / Docker Desktop), unless --no-db.
#    --iceberg also starts MinIO + an Iceberg REST catalog and seeds demo tables (needs python3),
#    plus a local SQL (SQLite) catalog at ~/.vibelens-dev/iceberg.
# 4. Launches the app with hot reload (npm run tauri dev). The first Rust build takes several minutes.
set -euo pipefail

cd "$(dirname "$0")/.."
WITH_DB=1
WITH_ICEBERG=0
for arg in "$@"; do
  case "$arg" in
    --no-db) WITH_DB=0 ;;
    --iceberg) WITH_ICEBERG=1 ;;
    *) echo "usage: $0 [--no-db] [--iceberg]" >&2; exit 2 ;;
  esac
done

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
ok() { printf '  \033[32m✓\033[0m %s\n' "$*"; }
warn() { printf '  \033[33m!\033[0m %s\n' "$*"; }
die() { printf '  \033[31m✗\033[0m %s\n' "$*" >&2; exit 1; }

bold "VibeLens · local dev"

# --- Xcode Command Line Tools (clang, linker, SDK) --------------------------------------------
if xcode-select -p >/dev/null 2>&1; then
  ok "Xcode Command Line Tools"
else
  warn "Xcode Command Line Tools missing; opening the installer. Re-run this script when it finishes."
  xcode-select --install || true
  exit 1
fi

# --- Rust ------------------------------------------------------------------------------------
[[ -f "$HOME/.cargo/env" ]] && source "$HOME/.cargo/env"
if ! command -v cargo >/dev/null 2>&1; then
  bold "Installing Rust (rustup, stable, minimal profile)…"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
  source "$HOME/.cargo/env"
fi
ok "$(rustc --version)"

# --- Node 24 LTS -------------------------------------------------------------------------------
export NVM_DIR="${NVM_DIR:-$HOME/.nvm}"
if [[ -s "$NVM_DIR/nvm.sh" ]]; then
  # shellcheck disable=SC1091
  source "$NVM_DIR/nvm.sh"
fi
node_ok() { command -v node >/dev/null 2>&1 && [[ "$(node -p 'process.versions.node')" == 24.* ]]; }
if ! node_ok; then
  if command -v nvm >/dev/null 2>&1; then
    bold "Installing Node 24 via nvm…"
    nvm install 24 >/dev/null
    nvm use 24 >/dev/null
  else
    die "Node 24 needed (found: $(node -v 2>/dev/null || echo none)). Install with: brew install node@24"
  fi
fi
ok "node $(node -v)"

# --- JS dependencies -------------------------------------------------------------------------
if [[ ! -d node_modules ]] || [[ package-lock.json -nt node_modules ]]; then
  bold "Installing npm dependencies…"
  npm ci --no-audit --no-fund
  touch node_modules
fi
ok "npm dependencies"

# --- Amazon RDS CA bundle (only needed for TLS to RDS/Aurora/ElastiCache) --------------------
if [[ ! -s src-tauri/resources/rds-global-bundle.pem ]]; then
  if ./scripts/fetch-rds-ca.sh >/dev/null 2>&1; then ok "RDS CA bundle"; else warn "RDS CA bundle not fetched (only needed for AWS TLS)"; fi
else
  ok "RDS CA bundle"
fi

# --- Seeded test databases -------------------------------------------------------------------
if (( WITH_DB )); then
  if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
    bold "Starting Postgres + Valkey (docker-compose.dev.yml)…"
    # Name the services: the Iceberg stack is opt-in (--iceberg).
    docker compose -f docker-compose.dev.yml up -d --wait postgres valkey >/dev/null
    if [[ "$(docker compose -f docker-compose.dev.yml exec -T valkey valkey-cli DBSIZE | tr -d '\r')" == "0" ]]; then
      ./scripts/seed/valkey-seed.sh | docker compose -f docker-compose.dev.yml exec -T valkey valkey-cli >/dev/null
    fi
    ok "Postgres  localhost:55432  db vibelens_test  user postgres / vibelens   (read-only user: vibelens_agent / agent)"
    ok "Valkey    localhost:56379  no auth"
    if (( WITH_ICEBERG )); then
      bold "Starting S3 storage (MinIO community fork) + Iceberg REST catalog…"
      docker compose -f docker-compose.dev.yml up -d minio iceberg-rest >/dev/null
      for _ in $(seq 1 60); do curl -fsS http://localhost:8181/v1/config >/dev/null 2>&1 && break; sleep 1; done
      curl -fsS http://localhost:8181/v1/config >/dev/null 2>&1 || die "Iceberg REST catalog didn't come up on :8181 (docker compose -f docker-compose.dev.yml logs iceberg-rest)"
      command -v python3 >/dev/null 2>&1 || die "python3 is needed to seed Iceberg tables (xcode-select --install provides it)"
      # Pinned: pyiceberg 0.11+ moved bucket/truncate transforms into the separate pyiceberg-core package.
      PYICEBERG_SPEC='pyiceberg[sql-sqlite,pyarrow,s3fs,pyiceberg-core]==0.12.0'
      venv="$HOME/.vibelens-dev/venv"
      [[ -x "$venv/bin/python" ]] || { mkdir -p "$HOME/.vibelens-dev"; python3 -m venv "$venv"; }
      if ! "$venv/bin/python" -c 'import pyiceberg, pyiceberg_core; assert pyiceberg.__version__ == "0.12.0"' >/dev/null 2>&1; then
        bold "Installing pyiceberg 0.12.0 into $venv (one-off)…"
        "$venv/bin/pip" install -q --upgrade "$PYICEBERG_SPEC" || die "pip install failed: $PYICEBERG_SPEC"
      fi
      # Seed unless the last table of the seed exists (a half-finished earlier run gets redone).
      if ! curl -fsS http://localhost:8181/v1/namespaces/shop/tables 2>/dev/null | grep -q '"events"'; then
        bold "Seeding Iceberg demo tables (REST catalog + S3)…"
        "$venv/bin/python" scripts/seed/iceberg-seed.py >/dev/null || die "Iceberg seed (REST) failed; re-run with: $venv/bin/python scripts/seed/iceberg-seed.py"
      fi
      sql_dir="$HOME/.vibelens-dev/iceberg"
      if [[ ! -f "$sql_dir/.seeded" ]]; then
        rm -rf "$sql_dir" && mkdir -p "$sql_dir/warehouse"   # our own scratch catalog: start clean
        "$venv/bin/python" scripts/seed/iceberg-seed.py --sql "$sql_dir/catalog.db" --warehouse "$sql_dir/warehouse" >/dev/null \
          || die "Iceberg seed (SQL catalog) failed; re-run with: $venv/bin/python scripts/seed/iceberg-seed.py --sql $sql_dir/catalog.db --warehouse $sql_dir/warehouse"
        touch "$sql_dir/.seeded"
      fi
      ok "Iceberg   REST http://localhost:8181 (auth none) · storage: "S3-compatible with access keys", endpoint http://localhost:9000, region us-east-1, path-style, admin / password"
      ok "Iceberg   SQL catalog sqlite:///$sql_dir/catalog.db, catalog name vg_test, warehouse file://$sql_dir/warehouse, storage Local (mark the connection dev)"
    fi
  else
    warn "Docker isn't running (start Rancher Desktop or Docker Desktop), so no test databases. Re-run, or pass --no-db."
  fi
fi

cat <<'EOF'

  In the app:  New connection → Postgres → host localhost, port 55432, database vibelens_test,
               Password auth (postgres / vibelens), TLS: Disable. Then the same for Valkey on 56379.
  AI agent:    AI settings → Ollama (local, no key) and set a model you've pulled, or add an Anthropic key.
  Expect:      macOS may ask to allow Documents access (worksheets live in ~/Documents/VibeLens) and,
               once, to allow Keychain access. Choose "Always Allow" and rebuilds won't ask again.

EOF
# Sign the dev binary with a stable identity so Keychain access survives rebuilds.
host_triple="$(rustc -vV | sed -n 's/^host: //p' | tr 'a-z-' 'A-Z_')"
export "CARGO_TARGET_${host_triple}_RUNNER=$PWD/scripts/dev-runner.sh"

bold "Launching VibeLens (first Rust build takes a few minutes)…"
exec npm run tauri dev
