#!/usr/bin/env bash
# Cargo runner for local macOS dev (dev-mac.sh sets it): signs the app binary, then runs it.
#
# The linker's ad-hoc signature changes with every build, and the Keychain ties "Always Allow" to
# the signature, so each rebuild would prompt again. Signing with one self-signed identity keeps
# the signature's designated requirement the same across rebuilds.
#
# The identity lives in its own keychain under ~/.vibelens-dev/codesign, created on first use. It
# is not trusted system-wide and is only added to the keychain search list while signing. Delete
# that directory to start over. If signing fails the app still runs, it just prompts as before.
set -uo pipefail

DIR="$HOME/.vibelens-dev/codesign"
KEYCHAIN="$DIR/dev.keychain-db"
IDENTIFIER="com.heinvandermerwe.vibelens.dev"

warn() { printf '  \033[33m!\033[0m dev signing: %s\n' "$*" >&2; }

create_identity() (
  set -e
  umask 077
  mkdir -p "$DIR"
  local work; work="$(mktemp -d)"
  trap 'rm -rf "$work"' EXIT
  cat >"$work/cert.cnf" <<'EOF'
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = VibeLens Dev
[ext]
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
basicConstraints = critical,CA:false
EOF
  openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
    -keyout "$work/key.pem" -out "$work/cert.pem" -config "$work/cert.cnf" 2>/dev/null
  # OpenSSL 3 needs -legacy for a bundle `security` can read; LibreSSL has no such flag.
  openssl pkcs12 -export -legacy -inkey "$work/key.pem" -in "$work/cert.pem" \
    -out "$work/id.p12" -passout pass:dev 2>/dev/null ||
    openssl pkcs12 -export -inkey "$work/key.pem" -in "$work/cert.pem" \
      -out "$work/id.p12" -passout pass:dev
  openssl rand -hex 24 >"$DIR/password"
  local password; password="$(cat "$DIR/password")"
  security create-keychain -p "$password" "$KEYCHAIN"
  security set-keychain-settings "$KEYCHAIN" # no auto-lock
  security unlock-keychain -p "$password" "$KEYCHAIN"
  security import "$work/id.p12" -k "$KEYCHAIN" -P dev -T /usr/bin/codesign >/dev/null
  security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$password" "$KEYCHAIN" >/dev/null
  openssl x509 -in "$work/cert.pem" -noout -fingerprint -sha1 | sed 's/.*=//; s/://g' >"$DIR/identity"
)

sign() {
  local binary="$1"
  if [[ ! -s "$DIR/identity" || ! -f "$KEYCHAIN" ]]; then
    rm -rf "$DIR"
    create_identity || { warn "could not create the identity"; rm -rf "$DIR"; return 1; }
  fi
  security unlock-keychain -p "$(cat "$DIR/password")" "$KEYCHAIN" || return 1

  # codesign only finds identities on the search list, so add ours just for this call.
  local keychains=() line status
  while IFS= read -r line; do
    line="${line#"${line%%[![:space:]]*}"}"; line="${line#\"}"; line="${line%\"}"
    keychains+=("$line")
  done < <(security list-keychains -d user)
  security list-keychains -d user -s "${keychains[@]}" "$KEYCHAIN" || return 1
  codesign -f -s "$(cat "$DIR/identity")" -i "$IDENTIFIER" "$binary" 2>/dev/null
  status=$?
  security list-keychains -d user -s "${keychains[@]}"
  return "$status"
}

if [[ "$(basename "$1")" == "vibelens" ]]; then
  sign "$1" || warn "binary left unsigned; Keychain will prompt after this rebuild"
fi
exec "$@"
