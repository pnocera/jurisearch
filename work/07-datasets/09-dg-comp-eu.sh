#!/usr/bin/env bash
set -euo pipefail

DEST_ROOT="${DEST_ROOT:-/mnt/models/opendata}"
DEST="$DEST_ROOT/DG_COMP_EU"
DG_COMP_URLS_FILE="${DG_COMP_URLS_FILE:-}"
mkdir -p "$DEST/downloads"

download() {
  local url="$1"
  local out="$2"
  mkdir -p "$(dirname "$out")"
  if [[ -s "$out" ]]; then
    echo "exists: $out"
    return 0
  fi
  echo "download: $url"
  curl -fL --retry 8 --retry-delay 5 --retry-all-errors -C - -o "$out.part" "$url"
  mv "$out.part" "$out"
}

cat > "$DEST/README.source.txt" <<'EOF'
Source: European Commission DG Competition case data.
No API key is expected for public data.europa.eu downloads, but the downloadable
asset URLs should be selected from the data.europa.eu dataset page because the
Commission publishes several case-data products.

Set DG_COMP_URLS_FILE to a newline-separated list of data.europa.eu distribution
URLs to download them into downloads/.

Discovery:
https://data.europa.eu/data/datasets?query=DG%20Competition%20case%20data
EOF

if [[ -z "$DG_COMP_URLS_FILE" ]]; then
  echo "DG_COMP_URLS_FILE is not set; wrote source notes only."
  exit 0
fi

while IFS= read -r url; do
  [[ -n "$url" && "$url" != \#* ]] || continue
  name="$(basename "${url%%\?*}")"
  [[ -n "$name" && "$name" != "/" ]] || name="dg-comp-$(date +%s)"
  download "$url" "$DEST/downloads/$name"
done < "$DG_COMP_URLS_FILE"

