#!/usr/bin/env bash
set -euo pipefail

DEST_ROOT="${DEST_ROOT:-/mnt/models/opendata}"
DEST="$DEST_ROOT/TED"
TED_START_YEAR="${TED_START_YEAR:-2024}"
TED_END_YEAR="${TED_END_YEAR:-$(date +%Y)}"
mkdir -p "$DEST/monthly"

download_optional() {
  local url="$1"
  local out="$2"
  mkdir -p "$(dirname "$out")"
  if [[ -s "$out" ]]; then
    echo "exists: $out"
    return 0
  fi
  echo "download: $url"
  local status=0
  curl -fL --retry 8 --retry-delay 5 --retry-all-errors -C - -o "$out.part" "$url" || status=$?
  if [[ "$status" -eq 0 ]]; then
    mv "$out.part" "$out"
  else
    rm -f "$out.part"
    echo "skip/unavailable: $url"
  fi
}

for year in $(seq "$TED_START_YEAR" "$TED_END_YEAR"); do
  for month in $(seq 1 12); do
    download_optional "https://ted.europa.eu/packages/monthly/$year-$month" \
      "$DEST/monthly/ted-$year-$month.zip"
  done
done

cat > "$DEST/README.source.txt" <<EOF
Source: TED data reuse monthly packages.
No API key is required.

Defaults:
TED_START_YEAR=$TED_START_YEAR
TED_END_YEAR=$TED_END_YEAR

Daily packages also exist at:
https://ted.europa.eu/packages/daily/{yyyynnnnn}

Reuse page:
https://ted.europa.eu/en/help/data-reuse
EOF

