#!/usr/bin/env bash
# Builds a release on this Mac and uploads it to a GitHub release, without GitHub Actions.
# It does what .github/workflows/release.yml does for the platforms a Mac can build:
#
#   macOS    built natively (universal: Apple Silicon + Intel), signed and notarised when the
#            APPLE_* variables of docs/RELEASING.md §5.2 are set, unsigned otherwise.
#   Linux    with --linux: .deb and .AppImage for x64, built in an Ubuntu 22.04 container
#            (Docker or Rancher Desktop). On Apple Silicon this runs under emulation and is slow.
#   Windows  not built. The installers need a Windows machine or a Windows runner.
#
# Usage, from anywhere in the repo:
#   VibeLens/scripts/release-local.sh            build, then create or update a DRAFT release
#   VibeLens/scripts/release-local.sh --linux    also build the Linux packages
#   VibeLens/scripts/release-local.sh --no-upload   build only; artifacts stay in dist-release/
#   VibeLens/scripts/release-local.sh --publish  publish the draft once you have tested it
#
# The version is read from src-tauri/tauri.conf.json (bump it first: docs/RELEASING.md §3).
# The tag is v<version>; it is created on HEAD if it does not exist, and must point at HEAD if
# it does. Nothing is uploaded from a working tree with uncommitted changes.
#
# Needs: Node 24, Rust with both Apple targets
#   (brew install rustup && rustup default stable && rustup target add aarch64-apple-darwin x86_64-apple-darwin),
# Xcode command line tools, and `gh` logged in to an account that can write to the repo.
set -euo pipefail

cd "$(dirname "$0")/.."
linux=false upload=true publish=false
for arg in "$@"; do
  case "$arg" in
    --linux) linux=true ;;
    --no-upload) upload=false ;;
    --publish) publish=true ;;
    -h | --help) sed -n '2,24p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $arg (see --help)" >&2; exit 2 ;;
  esac
done

die() { echo "error: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "$1 is not installed. $2"; }

version="$(node -p "require('./src-tauri/tauri.conf.json').version")"
tag="v$version"
repo="$(gh repo view --json nameWithOwner --jq .nameWithOwner)"

if $publish; then
  gh release view "$tag" --repo "$repo" >/dev/null || die "there is no release $tag to publish"
  flag=--latest
  [[ "$version" == *-* ]] && flag=--prerelease
  gh release edit "$tag" --repo "$repo" --draft=false "$flag"
  echo "published $tag: $(gh release view "$tag" --repo "$repo" --json url --jq .url)"
  exit 0
fi

# ---- checks before a long build -----------------------------------------------------------
need node "Install Node 24."
need cargo "brew install rustup && rustup default stable"
need rustup "brew install rustup"
[[ "$(node -p 'process.versions.node.split(".")[0]')" -ge 24 ]] || die "Node 24 or newer is required"

for f in package.json src-tauri/Cargo.toml Cargo.toml; do
  case "$f" in
    package.json) v="$(node -p "require('./package.json').version")" ;;
    *) v="$(grep -m1 '^version' "$f" | sed 's/.*"\(.*\)".*/\1/')" ;;
  esac
  [[ "$v" == "$version" ]] || die "$f has version $v, tauri.conf.json has $version (docs/RELEASING.md §3)"
done

if $upload; then
  need gh "brew install gh && gh auth login"
  [[ -z "$(git status --porcelain --untracked-files=no)" ]] || die "uncommitted changes: commit them first, the release must match a commit"
  head="$(git rev-parse HEAD)"
  if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
    [[ "$(git rev-parse "$tag^{commit}")" == "$head" ]] || die "tag $tag exists and is not HEAD. Never reuse a version: bump it"
  fi
  # Refuse to add files to a release that is already public.
  if state="$(gh release view "$tag" --repo "$repo" --json isDraft --jq .isDraft 2>/dev/null)"; then
    [[ "$state" == "true" ]] || die "release $tag is already published. Bump the version instead of replacing its files"
  fi
fi

out="$PWD/dist-release/$version"
rm -rf "$out" && mkdir -p "$out"

# ---- build: frontend checks, then macOS ----------------------------------------------------
npm ci
npm run typecheck
npm test
# bundle.resources = resources/*: the build fails without the RDS CA bundle. Refresh it; when the
# download is not reachable from this network, the copy committed in the repository is used.
if ! ./scripts/fetch-rds-ca.sh; then
  grep -q "BEGIN CERTIFICATE" src-tauri/resources/rds-global-bundle.pem 2>/dev/null \
    || die "could not download the RDS CA bundle and there is no committed copy"
  echo "warning: could not refresh the RDS CA bundle; using the committed copy." >&2
fi

rustup target add aarch64-apple-darwin x86_64-apple-darwin
if [[ -z "${APPLE_SIGNING_IDENTITY:-}" ]]; then
  echo "warning: APPLE_SIGNING_IDENTITY is not set: building an UNSIGNED macOS app (users need 'Open Anyway')." >&2
fi
npm run tauri build -- --target universal-apple-darwin
cp target/universal-apple-darwin/release/bundle/dmg/*.dmg "$out/"

# ---- build: Linux x64 in a container -------------------------------------------------------
if $linux; then
  need docker "Start Docker or Rancher Desktop."
  # A prerelease version has a hyphen, which RPM's Version field rejects: deb and AppImage only.
  bundles="deb,appimage"
  [[ "$version" == *-* ]] || bundles="deb,appimage,rpm"
  # The source is copied into the container so its node_modules and target stay off the Mac's.
  docker run --rm --platform linux/amd64 \
    -v "$PWD/..":/repo:ro -v "$out":/out -v vibelens-release-cargo:/root/.cargo/registry \
    -e CARGO_TARGET_DIR=/build/target ${SSL_CERT_FILE:+-e SSL_CERT_FILE=/ca.pem -e CARGO_HTTP_CAINFO=/ca.pem -e NODE_EXTRA_CA_CERTS=/ca.pem -v "$SSL_CERT_FILE":/ca.pem:ro} \
    ubuntu:22.04 bash -euo pipefail -c '
      export DEBIAN_FRONTEND=noninteractive
      apt-get update -qq
      apt-get install -y -qq curl ca-certificates build-essential file git pkg-config libssl-dev \
        libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev \
        librsvg2-dev libayatana-appindicator3-dev patchelf xdg-utils >/dev/null
      curl -fsSL https://deb.nodesource.com/setup_24.x | bash - >/dev/null
      apt-get install -y -qq nodejs >/dev/null
      curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal >/dev/null
      . "$HOME/.cargo/env"
      mkdir -p /build && cp -a /repo/VibeLens /build/app && cp /repo/LICENCE.md /build/ 2>/dev/null || true
      cd /build/app && rm -rf node_modules dist target
      npm ci
      ./scripts/fetch-rds-ca.sh
      # AppImage tooling runs AppImages itself, which needs FUSE that a container lacks.
      APPIMAGE_EXTRACT_AND_RUN=1 npm run tauri build -- --bundles '"$bundles"'
      find /build/target/release/bundle \( -name "*.deb" -o -name "*.AppImage" -o -name "*.rpm" \) -exec cp {} /out/ \;
    '
fi

# ---- checksums ----------------------------------------------------------------------------
(cd "$out" && rm -f SHA256SUMS.txt && shasum -a 256 -- * > SHA256SUMS.txt && cat SHA256SUMS.txt)
echo "artifacts: $out"
$upload || exit 0

# ---- draft release -------------------------------------------------------------------------
if ! git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
  git tag "$tag"
fi
git push origin "$tag"
if ! gh release view "$tag" --repo "$repo" >/dev/null 2>&1; then
  pre=()
  [[ "$version" == *-* ]] && pre=(--prerelease)
  gh release create "$tag" --repo "$repo" --draft "${pre[@]}" --verify-tag \
    --title "VibeLens $tag" --generate-notes
fi
gh release upload "$tag" --repo "$repo" --clobber "$out"/*
echo
echo "Draft release: $(gh release view "$tag" --repo "$repo" --json url --jq .url)"
echo "Built here: macOS$($linux && echo ', Linux x64'). Windows installers are NOT in this release."
echo "Test the downloads (docs/RELEASING.md §9), then: $0 --publish"
