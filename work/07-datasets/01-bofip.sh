#!/usr/bin/env bash
set -euo pipefail

DEST_ROOT="${DEST_ROOT:-/mnt/models/opendata}"
DEST="$DEST_ROOT/BOFIP"
mkdir -p "$DEST"

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

download "https://data.economie.gouv.fr/api/explore/v2.1/catalog/datasets/bofip-vigueur" \
  "$DEST/catalog-bofip-vigueur.json"

download "https://data.economie.gouv.fr/api/explore/v2.1/catalog/datasets/bofip-vigueur/exports/json" \
  "$DEST/bofip-vigueur.json"

download "https://data.economie.gouv.fr/api/explore/v2.1/catalog/datasets/bofip-vigueur/exports/csv?use_labels=true" \
  "$DEST/bofip-vigueur.csv"

download "https://data.economie.gouv.fr/api/explore/v2.1/catalog/datasets/bofip-impots/attachments/bofip_documentation_pdf" \
  "$DEST/bofip-documentation.pdf"

cat > "$DEST/README.source.txt" <<'EOF'
Source: BOFiP-Impots publications en vigueur.
No API key is required for these Opendatasoft exports.

Primary dataset:
https://www.data.gouv.fr/datasets/bofip-impots-publications-en-vigueur
https://data.economie.gouv.fr/explore/dataset/bofip-vigueur
EOF

