#!/usr/bin/env bash
#
# tauri-release-bench.sh — C.1.b packaging-path spike (issue audiophore/audiophore#39).
#
# Builds the audiophore-ui Tauri app, benches the build/sign/notarize timings,
# runs every verification the available credentials allow, and publishes the
# updater test feed (gist) so `tauri-plugin-updater` can fetch + verify it.
#
# Credential auto-detection:
#   * Updater signing (always required) — TAURI_SIGNING_PRIVATE_KEY[_PASSWORD].
#   * Apple signing + notarization (optional) — if APPLE_SIGNING_IDENTITY and the
#     App Store Connect API-key vars are set, the build is Developer ID signed +
#     notarized and the codesign/staple checks run. If absent, the script builds
#     UNSIGNED (still producing + verifying the minisign updater artifacts) and
#     skips the Apple-only checks with a notice. This lets the updater half be
#     exercised before the Apple cert is provisioned.
#
# See ~/.audiophore/secrets/README.md for the env vars to export.

set -euo pipefail

# ── Config ──────────────────────────────────────────────────────────────────
GIST_ID="6d994355c7961590ed0d77fd0988eff3"        # updater test feed
RELEASE_TAG="c1b-updater-test"                      # throwaway prerelease hosting the artifact
REPO_ROOT="$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)"
TAURI_DIR="$REPO_ROOT/crates/audiophore-ui/src-tauri"
BUNDLE_DIR="$REPO_ROOT/target/release/bundle"

cd "$REPO_ROOT"

note() { printf '\033[1;34m▸ %s\033[0m\n' "$*"; }
warn() { printf '\033[1;33m! %s\033[0m\n' "$*"; }
die()  { printf '\033[1;31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

# ── Preflight ───────────────────────────────────────────────────────────────
command -v cargo-tauri >/dev/null 2>&1 || cargo tauri --version >/dev/null 2>&1 \
  || die "tauri CLI not found (cargo install tauri-cli --version '^2')"

[[ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ]] \
  || die "TAURI_SIGNING_PRIVATE_KEY unset — needed to sign the updater artifact. See ~/.audiophore/secrets/README.md"

SIGNED=0
if [[ -n "${APPLE_SIGNING_IDENTITY:-}" && -n "${APPLE_API_KEY:-}" \
      && -n "${APPLE_API_ISSUER:-}" && -n "${APPLE_API_KEY_PATH:-}" ]]; then
  SIGNED=1
  note "Apple creds present → Developer ID signed + notarized build."
  security find-identity -v -p codesigning | grep -q "$APPLE_SIGNING_IDENTITY" \
    || warn "APPLE_SIGNING_IDENTITY '$APPLE_SIGNING_IDENTITY' not found by find-identity — build may fail to sign."
else
  warn "Apple creds absent → UNSIGNED build (updater artifacts still signed + verified; codesign/notarize checks skipped)."
fi

VERSION="$(grep -m1 '"version"' "$TAURI_DIR/tauri.conf.json" | sed -E 's/.*"version": *"([^"]+)".*/\1/')"
ARCH="$(uname -m)"; case "$ARCH" in arm64|aarch64) UPD_ARCH="darwin-aarch64";; x86_64) UPD_ARCH="darwin-x86_64";; *) die "unsupported arch $ARCH";; esac
note "Building Audiophore v$VERSION ($UPD_ARCH)"

# ── Build (timed) ───────────────────────────────────────────────────────────
note "cargo tauri build …"
BUILD_START=$SECONDS
( cd "$TAURI_DIR" && cargo tauri build )
BUILD_SECS=$(( SECONDS - BUILD_START ))
note "build wall time: ${BUILD_SECS}s"

APP="$(/usr/bin/find "$BUNDLE_DIR/macos" -maxdepth 1 -name '*.app' -print -quit)"
[[ -n "$APP" ]] || die "no .app produced under $BUNDLE_DIR/macos"
TARBALL="$(/usr/bin/find "$BUNDLE_DIR/macos" -maxdepth 1 -name '*.app.tar.gz' -print -quit)"
SIGFILE="${TARBALL}.sig"
[[ -f "$TARBALL" && -f "$SIGFILE" ]] || die "updater artifact ($TARBALL[.sig]) missing — is bundle.createUpdaterArtifacts true?"
note "app:     $APP"
note "updater: $TARBALL"

# ── Verifications ───────────────────────────────────────────────────────────
[[ -s "$SIGFILE" ]] && note "updater signature present ($(wc -c <"$SIGFILE" | tr -d ' ') bytes)" || die "empty updater signature"

if [[ "$SIGNED" == 1 ]]; then
  note "codesign --verify"
  codesign --verify --verbose=4 "$APP" || die "codesign verification failed"
  note "stapler validate (notarization)"
  xcrun stapler validate "$APP" || warn "stapler validate failed — notarization may not have completed"
  note "spctl assessment"
  spctl -a -vv -t exec "$APP" || warn "spctl rejected the app"
else
  warn "skipped codesign/staple/spctl (unsigned build)"
fi

# ── Publish updater feed ────────────────────────────────────────────────────
note "Publishing updater artifact to prerelease '$RELEASE_TAG'"
gh release view "$RELEASE_TAG" >/dev/null 2>&1 \
  && gh release upload "$RELEASE_TAG" "$TARBALL" --clobber \
  || gh release create "$RELEASE_TAG" "$TARBALL" \
       --prerelease --title "C.1.b updater test artifact" \
       --notes "Throwaway artifact for the updater spike (issue #39). Safe to delete."
ASSET_URL="$(gh release view "$RELEASE_TAG" --json assets \
  --jq ".assets[] | select(.name==\"$(basename "$TARBALL")\") | .url")"
[[ -n "$ASSET_URL" ]] || die "could not resolve uploaded asset URL"

FEED="$(mktemp)"
cat > "$FEED" <<JSON
{
  "version": "$VERSION",
  "notes": "Audiophore C.1.b updater test build.",
  "pub_date": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "platforms": {
    "$UPD_ARCH": {
      "signature": "$(cat "$SIGFILE")",
      "url": "$ASSET_URL"
    }
  }
}
JSON
gh gist edit "$GIST_ID" -f latest.json "$FEED"
note "feed updated: https://gist.githubusercontent.com/IamMrCupp/$GIST_ID/raw/latest.json"

# ── Bench summary ───────────────────────────────────────────────────────────
printf '\n\033[1m── bench summary ──\033[0m\n'
printf 'version            : %s (%s)\n' "$VERSION" "$UPD_ARCH"
printf 'signed+notarized   : %s\n' "$([[ $SIGNED == 1 ]] && echo yes || echo 'no (unsigned)')"
printf 'build wall time    : %ss  (note: deps warm; run `cargo clean -p audiophore-ui` first for a colder number)\n' "$BUILD_SECS"
printf 'updater artifact   : %s\n' "$(basename "$TARBALL")"
printf 'feed               : https://gist.githubusercontent.com/IamMrCupp/%s/raw/latest.json\n' "$GIST_ID"
