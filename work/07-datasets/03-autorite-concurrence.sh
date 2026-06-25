#!/usr/bin/env bash
set -euo pipefail

DEST_ROOT="${DEST_ROOT:-/mnt/models/opendata}"
DEST="$DEST_ROOT/AUTORITE_CONCURRENCE"
SLUG="decisions-publiees-par-lautorite-de-la-concurrence-depuis-1988"
API_URL="https://www.data.gouv.fr/api/1/datasets/$SLUG/"
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

download "$API_URL" "$DEST/dataset-metadata.json"

python3 - "$DEST/dataset-metadata.json" > "$DEST/manifest.tsv" <<'PY'
import json
import re
import sys

with open(sys.argv[1], "r", encoding="utf-8") as f:
    data = json.load(f)

def clean(name: str) -> str:
    name = re.sub(r"[^A-Za-z0-9._-]+", "-", name.strip()).strip("-")
    return name or "resource"

for resource in data.get("resources", []):
    url = resource.get("latest") or resource.get("url")
    if not url:
        continue
    fmt = (resource.get("format") or "").lower()
    title = resource.get("title") or resource.get("id") or url.rsplit("/", 1)[-1]
    suffix = "." + fmt if fmt and not clean(title).lower().endswith("." + fmt) else ""
    print(f"{url}\t{clean(title)}{suffix}")
PY

while IFS=$'\t' read -r url name; do
  [[ -n "${url:-}" && -n "${name:-}" ]] || continue
  download "$url" "$DEST/$name"
done < "$DEST/manifest.tsv"

cat > "$DEST/README.source.txt" <<EOF
Source: Decisions publiees par l'Autorite de la concurrence depuis 1988.
No API key is required for data.gouv resource downloads.

Dataset:
https://www.data.gouv.fr/datasets/$SLUG
EOF

