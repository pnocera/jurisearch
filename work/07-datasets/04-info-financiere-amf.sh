#!/usr/bin/env bash
set -euo pipefail

DEST_ROOT="${DEST_ROOT:-/mnt/models/opendata}"
DEST="$DEST_ROOT/INFO_FINANCIERE"
DATASET_ID="${INFO_FINANCIERE_DATASET_ID:-flux-amf-new-prod}"
BASE="https://www.info-financiere.gouv.fr/api/explore/v2.1/catalog/datasets/$DATASET_ID"
DOWNLOAD_DOCUMENTS="${DOWNLOAD_DOCUMENTS:-0}"
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

download "$BASE" "$DEST/catalog-$DATASET_ID.json"
download "$BASE/exports/json" "$DEST/$DATASET_ID.json"
download "$BASE/exports/csv?use_labels=true" "$DEST/$DATASET_ID.csv"

if [[ "$DOWNLOAD_DOCUMENTS" == "1" ]]; then
  python3 - "$DEST/$DATASET_ID.json" > "$DEST/document-urls.tsv" <<'PY'
import json
import re
import sys

with open(sys.argv[1], "r", encoding="utf-8") as f:
    payload = json.load(f)

def clean(name: str) -> str:
    return re.sub(r"[^A-Za-z0-9._-]+", "-", name.strip()).strip("-") or "document"

rows = payload if isinstance(payload, list) else payload.get("results", [])
seen = set()
for i, row in enumerate(rows, 1):
    fields = row.get("fields", row) if isinstance(row, dict) else {}
    url = fields.get("url_de_recuperation") or fields.get("url") or fields.get("lien")
    if not url or url in seen:
        continue
    seen.add(url)
    uin = fields.get("uin_idt_uin") or fields.get("id") or str(i)
    date = fields.get("uin_dat_amf") or fields.get("date") or "unknown-date"
    print(f"{url}\t{clean(str(date))}-{clean(str(uin))}")
PY

  while IFS=$'\t' read -r url name; do
    [[ -n "${url:-}" && -n "${name:-}" ]] || continue
    download "$url" "$DEST/documents/$name"
  done < "$DEST/document-urls.tsv"
fi

cat > "$DEST/README.source.txt" <<EOF
Source: API Info Financiere, AMF-regulated issuer publications.
No API key is required. The public service documents a 10,000 calls/IP/day limit.

Default behavior downloads metadata exports only.
Set DOWNLOAD_DOCUMENTS=1 to download each document URL from url_de_recuperation.

Dataset id: $DATASET_ID
API:
https://www.info-financiere.gouv.fr/api/explore/v2.0
EOF

