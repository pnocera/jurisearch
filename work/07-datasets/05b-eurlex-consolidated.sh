#!/usr/bin/env bash
set -euo pipefail

DEST_ROOT="${DEST_ROOT:-/mnt/models/opendata}"
DEST="$DEST_ROOT/EURLEX"
CELEX_IDS_FILE="${CELEX_IDS_FILE:-work/07-datasets/eurlex-business-celex.txt}"
EURLEX_CELEX_LANG="${EURLEX_CELEX_LANG:-FRA}"
EURLEX_CONSOLIDATED_MODE="${EURLEX_CONSOLIDATED_MODE:-latest}"
mkdir -p "$DEST/work-rdf" "$DEST/consolidated-$EURLEX_CELEX_LANG-xhtml"

download() {
  local url="$1"
  local out="$2"
  mkdir -p "$(dirname "$out")"
  if [[ -s "$out" ]]; then
    echo "exists: $out"
    return 0
  fi
  echo "download: $url"
  curl -fL --retry 4 --retry-delay 3 --retry-all-errors -C - \
    -A "jurisearch-dataset-script/1.0" \
    -H "Accept: application/xhtml+xml,text/html,application/xml;q=0.9,*/*;q=0.8" \
    -o "$out.part" "$url"
  if [[ ! -s "$out.part" ]]; then
    rm -f "$out.part"
    echo "empty download: $url" >&2
    return 1
  fi
  mv "$out.part" "$out"
}

: > "$DEST/consolidated-manifest.tsv"
: > "$DEST/consolidated-failures.tsv"

while IFS= read -r celex; do
  [[ -n "$celex" && "$celex" != \#* ]] || continue
  rdf="$DEST/work-rdf/$celex.rdf"
  download "https://publications.europa.eu/resource/celex/$celex" "$rdf"

  python3 - "$rdf" "$celex" "$EURLEX_CONSOLIDATED_MODE" <<'PY' > "$DEST/consolidated-candidates.tmp"
import re
import sys

rdf_path, celex, mode = sys.argv[1], sys.argv[2], sys.argv[3]
text = open(rdf_path, "r", encoding="utf-8", errors="replace").read()
base = "0" + celex[1:]
values = sorted(set(re.findall(re.escape(base) + r"-[0-9]{8}", text)))
if mode == "latest" and values:
    values = [values[-1]]
for value in values:
    print(value)
PY

  if [[ ! -s "$DEST/consolidated-candidates.tmp" ]]; then
    printf '%s\t%s\n' "$celex" "no consolidated CELEX found in RDF" >> "$DEST/consolidated-failures.tsv"
    continue
  fi

  while IFS= read -r consolidated; do
    [[ -n "$consolidated" ]] || continue
    out="$DEST/consolidated-$EURLEX_CELEX_LANG-xhtml/$consolidated.xhtml"
    url="https://publications.europa.eu/resource/celex/$consolidated.$EURLEX_CELEX_LANG"
    if download "$url" "$out"; then
      printf '%s\t%s\t%s\t%s\n' "$celex" "$consolidated" "$url" "$out" >> "$DEST/consolidated-manifest.tsv"
    else
      printf '%s\t%s\t%s\n' "$celex" "$consolidated" "$url" >> "$DEST/consolidated-failures.tsv"
    fi
  done < "$DEST/consolidated-candidates.tmp"
done < "$CELEX_IDS_FILE"

rm -f "$DEST/consolidated-candidates.tmp"

cat > "$DEST/README.consolidated.txt" <<EOF
Source: Publications Office Cellar CELEX resources.
Mode: EURLEX_CONSOLIDATED_MODE=$EURLEX_CONSOLIDATED_MODE
Language: EURLEX_CELEX_LANG=$EURLEX_CELEX_LANG

consolidated-manifest.tsv columns:
original_celex, consolidated_celex, url, local_path

consolidated-failures.tsv records original CELEX ids where no consolidated
version was found or the Cellar content URL failed.
EOF

