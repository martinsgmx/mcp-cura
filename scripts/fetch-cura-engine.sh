#!/usr/bin/env bash
# Extract a CuraEngine executable from an Ultimaker Cura AppImage.
#
# Usage:
#   scripts/fetch-cura-engine.sh [path-to-appimage]
#
# Environment:
#   CURA_ENGINE_APPIMAGE_URL  Download URL used when no local AppImage is given.
#
# The extracted binary is copied to the workspace-local path `.cura-engine/CuraEngine`.

set -euo pipefail

ENGINE_NAME="CuraEngine"
DEST_DIR=".cura-engine"
DEST="$DEST_DIR/$ENGINE_NAME"

if [[ $# -ge 1 ]]; then
  APPIMAGE="$1"
else
  APPIMAGE="${TMPDIR:-/tmp}/CuraEngine.AppImage"
  if [[ -z "${CURA_ENGINE_APPIMAGE_URL:-}" ]]; then
    echo "error: provide a local AppImage path or set CURA_ENGINE_APPIMAGE_URL" >&2
    exit 2
  fi
  curl -fL --retry 2 -o "$APPIMAGE" "$CURA_ENGINE_APPIMAGE_URL"
fi

if [[ ! -f "$APPIMAGE" ]]; then
  echo "error: AppImage not found at $APPIMAGE" >&2
  exit 2
fi

WORK="$(mktemp -d)"
if command -v appimage-extract.sh >/dev/null 2>&1; then
  appimage-extract.sh --appimage-extract "$APPIMAGE" >/dev/null 2>&1 || true
  if [[ -d squashfs-root ]]; then
    EXTRACTED="squashfs-root"
  else
    EXTRACTED="$WORK/extracted"
    unsquashfs -d "$EXTRACTED" "$APPIMAGE" >/dev/null 2>&1
  fi
else
  EXTRACTED="$WORK/extracted"
  unsquashfs -d "$EXTRACTED" "$APPIMAGE" >/dev/null 2>&1
fi

FOUND=$(find "$EXTRACTED" -type f -name "$ENGINE_NAME" -perm -u+x 2>/dev/null | head -n1 || true)
if [[ -z "$FOUND" ]]; then
  echo "error: no executable named $ENGINE_NAME found in the AppImage" >&2
  exit 1
fi

mkdir -p "$DEST_DIR"
cp "$FOUND" "$DEST"
chmod +x "$DEST"
echo "CuraEngine installed at $DEST"
