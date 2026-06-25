#!/usr/bin/env bash
set -euo pipefail

DEST_ROOT="${DEST_ROOT:-/mnt/models/opendata}"
DEST="$DEST_ROOT/EURLEX"
CELEX_IDS_FILE="${CELEX_IDS_FILE:-work/07-datasets/eurlex-business-celex.txt}"
EURLEX_CJEU_DOWNLOAD="${EURLEX_CJEU_DOWNLOAD:-0}"
EURLEX_CELEX_LANG="${EURLEX_CELEX_LANG:-FRA}"
mkdir -p "$DEST/relations" "$DEST/cjeu-rdf" "$DEST/cjeu-$EURLEX_CELEX_LANG-xhtml"

if [[ ! -s "$DEST/relations/case-law-links.tsv" ]]; then
  echo "case-law-links.tsv missing; running 05c-eurlex-relations.sh first."
  CELEX_IDS_FILE="$CELEX_IDS_FILE" EURLEX_CELEX_LANG="$EURLEX_CELEX_LANG" "$(dirname "$0")/05c-eurlex-relations.sh"
fi

python3 - "$DEST/relations/case-law-links.tsv" > "$DEST/cjeu-celex.txt" <<'PY'
import re
import sys

seen = set()
for line in open(sys.argv[1], encoding="utf-8"):
    if line.startswith("source_celex"):
        continue
    parts = line.rstrip("\n").split("\t")
    if len(parts) < 3:
        continue
    target = parts[2]
    if re.match(r"6[0-9A-Z()]+$", target) and target not in seen:
        seen.add(target)
        print(target)
PY

cat > "$DEST/cjeu-business-manifest.tsv" <<EOF
case_celex	source
EOF
while IFS= read -r case_celex; do
  [[ -n "$case_celex" ]] || continue
  printf '%s\t%s\n' "$case_celex" "Cellar relation from business-law seed CELEX list" >> "$DEST/cjeu-business-manifest.tsv"
done < "$DEST/cjeu-celex.txt"

if [[ "$EURLEX_CJEU_DOWNLOAD" == "1" ]]; then
  while IFS= read -r case_celex; do
    [[ -n "$case_celex" ]] || continue
    curl -fL --retry 3 --retry-delay 3 --retry-all-errors -C - \
      -A "jurisearch-dataset-script/1.0" \
      -H "Accept: application/rdf+xml,application/xml;q=0.9,*/*;q=0.8" \
      -o "$DEST/cjeu-rdf/$case_celex.rdf.part" \
      "https://publications.europa.eu/resource/celex/$case_celex" && \
      mv "$DEST/cjeu-rdf/$case_celex.rdf.part" "$DEST/cjeu-rdf/$case_celex.rdf" || true

    curl -fL --retry 2 --retry-delay 3 -C - \
      -A "jurisearch-dataset-script/1.0" \
      -H "Accept: application/xhtml+xml,text/html,application/xml;q=0.9,*/*;q=0.8" \
      -o "$DEST/cjeu-$EURLEX_CELEX_LANG-xhtml/$case_celex.xhtml.part" \
      "https://publications.europa.eu/resource/celex/$case_celex.$EURLEX_CELEX_LANG" && \
      mv "$DEST/cjeu-$EURLEX_CELEX_LANG-xhtml/$case_celex.xhtml.part" "$DEST/cjeu-$EURLEX_CELEX_LANG-xhtml/$case_celex.xhtml" || true
  done < "$DEST/cjeu-celex.txt"
else
  echo "Set EURLEX_CJEU_DOWNLOAD=1 to download case RDF/XHTML after reviewing $DEST/cjeu-business-manifest.tsv"
fi

