#!/usr/bin/env bash
set -euo pipefail

DEST_ROOT="${DEST_ROOT:-/mnt/models/opendata}"
DEST="$DEST_ROOT/EURLEX"
CELEX_IDS_FILE="${CELEX_IDS_FILE:-work/07-datasets/eurlex-business-celex.txt}"
EURLEX_CJEU_DOWNLOAD="${EURLEX_CJEU_DOWNLOAD:-0}"
EURLEX_CELEX_LANG="${EURLEX_CELEX_LANG:-FRA}"
mkdir -p "$DEST/relations" "$DEST/cjeu-rdf" "$DEST/cjeu-$EURLEX_CELEX_LANG-xhtml"

download_optional() {
  local url="$1"
  local out="$2"
  local accept="$3"
  local retries="$4"

  mkdir -p "$(dirname "$out")"
  if [[ -s "$out" ]]; then
    return 0
  fi

  if curl -fsSL --retry "$retries" --retry-delay 3 --retry-all-errors -C - \
    -A "jurisearch-dataset-script/1.0" \
    -H "Accept: $accept" \
    -o "$out.part" "$url"; then
    if [[ -s "$out.part" ]]; then
      mv "$out.part" "$out"
      return 0
    fi
  fi

  rm -f "$out.part"
  return 1
}

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
  failures="$DEST/cjeu-download-failures.tsv"
  : > "$failures"
  total="$(wc -l < "$DEST/cjeu-celex.txt" | tr -d ' ')"
  current=0
  downloaded_rdf=0
  downloaded_xhtml=0
  skipped=0
  failed=0

  while IFS= read -r case_celex; do
    [[ -n "$case_celex" ]] || continue
    current=$((current + 1))

    rdf_out="$DEST/cjeu-rdf/$case_celex.rdf"
    xhtml_out="$DEST/cjeu-$EURLEX_CELEX_LANG-xhtml/$case_celex.xhtml"
    rdf_exists=0
    xhtml_exists=0
    [[ -s "$rdf_out" ]] && rdf_exists=1
    [[ -s "$xhtml_out" ]] && xhtml_exists=1

    if [[ "$rdf_exists" == "1" && "$xhtml_exists" == "1" ]]; then
      skipped=$((skipped + 1))
      if (( current % 100 == 0 || current == total )); then
        printf 'progress %s/%s skipped=%s downloaded_rdf=%s downloaded_xhtml=%s failed=%s\n' \
          "$current" "$total" "$skipped" "$downloaded_rdf" "$downloaded_xhtml" "$failed"
      fi
      continue
    fi

    printf 'download %s/%s %s' "$current" "$total" "$case_celex"

    if [[ "$rdf_exists" == "1" ]]; then
      printf ' rdf=exists'
    elif download_optional \
      "https://publications.europa.eu/resource/celex/$case_celex" \
      "$rdf_out" \
      "application/rdf+xml,application/xml;q=0.9,*/*;q=0.8" \
      3; then
      downloaded_rdf=$((downloaded_rdf + 1))
      printf ' rdf=ok'
    else
      failed=$((failed + 1))
      printf ' rdf=failed'
      printf '%s\t%s\t%s\n' "$case_celex" "rdf" "https://publications.europa.eu/resource/celex/$case_celex" >> "$failures"
    fi

    if [[ "$xhtml_exists" == "1" ]]; then
      printf ' xhtml=exists'
    elif download_optional \
      "https://publications.europa.eu/resource/celex/$case_celex.$EURLEX_CELEX_LANG" \
      "$xhtml_out" \
      "application/xhtml+xml,text/html,application/xml;q=0.9,*/*;q=0.8" \
      2; then
      downloaded_xhtml=$((downloaded_xhtml + 1))
      printf ' xhtml=ok'
    else
      failed=$((failed + 1))
      printf ' xhtml=failed'
      printf '%s\t%s\t%s\n' "$case_celex" "xhtml" "https://publications.europa.eu/resource/celex/$case_celex.$EURLEX_CELEX_LANG" >> "$failures"
    fi

    printf '\n'
  done < "$DEST/cjeu-celex.txt"

  printf 'done total=%s skipped=%s downloaded_rdf=%s downloaded_xhtml=%s failed=%s failures=%s\n' \
    "$total" "$skipped" "$downloaded_rdf" "$downloaded_xhtml" "$failed" "$failures"
else
  echo "Set EURLEX_CJEU_DOWNLOAD=1 to download case RDF/XHTML after reviewing $DEST/cjeu-business-manifest.tsv"
fi
