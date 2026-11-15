#!/usr/bin/env bash
# Downloads the Amazon RDS global CA bundle into the Tauri resources folder so that
# "Verify full" TLS works against RDS/Aurora (their CAs are not in OS trust stores).
# Source: https://docs.aws.amazon.com/AmazonRDS/latest/UserGuide/UsingWithRDS.SSL.html
set -euo pipefail
cd "$(dirname "$0")/.."
dest="src-tauri/resources/rds-global-bundle.pem"
curl -fsSL https://truststore.pki.rds.amazonaws.com/global/global-bundle.pem -o "$dest.tmp"
grep -q "BEGIN CERTIFICATE" "$dest.tmp" || { echo "download did not look like a PEM bundle" >&2; rm -f "$dest.tmp"; exit 1; }
mv "$dest.tmp" "$dest"
echo "✓ $(grep -c 'BEGIN CERTIFICATE' "$dest") certificates → $dest"
