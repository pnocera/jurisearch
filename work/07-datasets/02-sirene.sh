#!/usr/bin/env bash
set -euo pipefail

DEST_ROOT="${DEST_ROOT:-/mnt/models/opendata}"
DEST="$DEST_ROOT/SIRENE"
SLUG="base-sirene-des-entreprises-et-de-leurs-etablissements-siren-siret"
API_URL="https://www.data.gouv.fr/api/1/datasets/$SLUG/"
SIRENE_FORMATS="${SIRENE_FORMATS:-parquet}"
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

python3 - "$DEST/dataset-metadata.json" "$SIRENE_FORMATS" > "$DEST/manifest.tsv" <<'PY'
import json
import re
import sys

metadata_path, wanted_formats = sys.argv[1], {x.lower() for x in sys.argv[2].split()}
with open(metadata_path, "r", encoding="utf-8") as f:
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
    haystack = " ".join(str(resource.get(k) or "") for k in ("title", "description", "url", "latest")).lower()
    if wanted_formats and fmt not in wanted_formats and not any(f".{fmt_}" in haystack for fmt_ in wanted_formats):
        continue
    if "stock" not in haystack and "sirene" not in haystack:
        continue
    suffix = "." + fmt if fmt and not clean(title).lower().endswith("." + fmt) else ""
    print(f"{url}\t{clean(title)}{suffix}")
PY

while IFS=$'\t' read -r url name; do
  [[ -n "${url:-}" && -n "${name:-}" ]] || continue
  download "$url" "$DEST/$name"
done < "$DEST/manifest.tsv"

cat > "$DEST/README.source.txt" <<EOF
Source: Base Sirene des entreprises et de leurs etablissements.
No API key is required for the public monthly stock files.

Default filter: SIRENE_FORMATS="$SIRENE_FORMATS"
Set SIRENE_FORMATS="parquet zip" to also download ZIP/CSV stock resources.

Dataset:
https://www.data.gouv.fr/datasets/$SLUG
EOF

