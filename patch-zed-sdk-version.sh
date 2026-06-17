#!/usr/bin/env bash
set -euo pipefail

source_app="/Applications/Zed Nightly.app"
dest_app="/tmp/Zed Nightly SDK27.app"
minos="11.0"
sdk="27.0"
ld_version="1328.2"
executable="zed"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
entitlements="$repo_root/crates/zed/resources/zed.entitlements"
resign_only=false

usage() {
  cat <<EOF
Usage: ${0##*/} [options]

Copies a Zed.app bundle, patches the main executable's LC_BUILD_VERSION SDK
metadata with vtool, ad-hoc re-signs the app, and verifies the result.

Options:
  --source PATH        Source .app bundle (default: $source_app)
  --dest PATH          Destination .app bundle (default: $dest_app)
  --minos VERSION      LC_BUILD_VERSION minimum macOS version (default: $minos)
  --sdk VERSION        LC_BUILD_VERSION SDK version (default: $sdk)
  --ld VERSION         LC_BUILD_VERSION ld tool version (default: $ld_version)
  --executable NAME    Main executable name in Contents/MacOS (default: $executable)
  --entitlements PATH  Entitlements plist for codesign (default: $entitlements)
  --resign-only        Only copy and ad-hoc re-sign; do not modify LC_BUILD_VERSION
  -h, --help           Show this help

Example:
  ${0##*/}
  open -n "$dest_app"
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --source)
      source_app="$2"
      shift 2
      ;;
    --dest)
      dest_app="$2"
      shift 2
      ;;
    --minos)
      minos="$2"
      shift 2
      ;;
    --sdk)
      sdk="$2"
      shift 2
      ;;
    --ld)
      ld_version="$2"
      shift 2
      ;;
    --executable)
      executable="$2"
      shift 2
      ;;
    --entitlements)
      entitlements="$2"
      shift 2
      ;;
    --resign-only)
      resign_only=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

require_tool() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Required tool not found: $1" >&2
    exit 1
  fi
}

require_tool cp
require_tool mv
require_tool chmod
require_tool xattr
require_tool vtool
require_tool codesign
require_tool shasum

if [[ ! -d "$source_app" ]]; then
  echo "Source app does not exist: $source_app" >&2
  exit 1
fi

if [[ ! -f "$entitlements" ]]; then
  echo "Entitlements file does not exist: $entitlements" >&2
  exit 1
fi

source_bin="$source_app/Contents/MacOS/$executable"
dest_bin="$dest_app/Contents/MacOS/$executable"

if [[ ! -f "$source_bin" ]]; then
  echo "Source executable does not exist: $source_bin" >&2
  exit 1
fi

echo "Source app:      $source_app"
echo "Destination app: $dest_app"
echo "Executable:      Contents/MacOS/$executable"
echo "Target minOS:    $minos"
echo "Target SDK:      $sdk"
echo "Target ld:       $ld_version"
echo "Resign only:     $resign_only"
echo

echo "Original build version:"
vtool -show-build "$source_bin"
echo

echo "Original SHA-256:"
shasum -a 256 "$source_bin"
echo

rm -rf "$dest_app"
cp -R "$source_app" "$dest_app"
xattr -cr "$dest_app" || true

if [[ "$resign_only" == "false" ]]; then
  patched_bin="$dest_bin.sdk${sdk}"
  vtool \
    -set-build-version macos "$minos" "$sdk" -tool ld "$ld_version" \
    -replace \
    -output "$patched_bin" \
    "$dest_bin"

  mv "$patched_bin" "$dest_bin"
  chmod +x "$dest_bin"
else
  echo "Skipping LC_BUILD_VERSION modification (--resign-only)."
fi

codesign \
  --force \
  --deep \
  --entitlements "$entitlements" \
  --sign - \
  "$dest_app"

echo
echo "Patched build version:"
vtool -show-build "$dest_bin"
echo

echo "Patched SHA-256:"
shasum -a 256 "$dest_bin"
echo

echo "Signature:"
codesign -dv --verbose=2 "$dest_app" 2>&1 | sed -n '1,40p'
echo

echo "Verification:"
codesign --verify --deep --strict --verbose=2 "$dest_app"
echo

echo "Created: $dest_app"
echo "Launch with: open -n \"$dest_app\""
