#!/usr/bin/env bash
set -euo pipefail

DEST_ROOT="${DEST_ROOT:-/mnt/models/opendata}"
DEST="$DEST_ROOT/EURLEX"
EURLEX_URLS_FILE="${EURLEX_URLS_FILE:-}"
CELEX_IDS_FILE="${CELEX_IDS_FILE:-}"
mkdir -p "$DEST/dumps" "$DEST/celex-fr-html"

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
Source: EUR-Lex reuse services.
No API key is required for public EUR-Lex reuse services, but the full dump URLs
are selected from the EUR-Lex reuse portal rather than a stable one-size script.

Set EURLEX_URLS_FILE to a newline-separated list of official EUR-Lex/Cellar dump
URLs to download them into dumps/.

Optionally set CELEX_IDS_FILE to a newline-separated list of CELEX identifiers to
download French EUR-Lex HTML snapshots for targeted business-law corpora.

Reuse page:
https://eur-lex.europa.eu/content/tools/data-reuse.html
EOF

if [[ -n "$EURLEX_URLS_FILE" ]]; then
  while IFS= read -r url; do
    [[ -n "$url" && "$url" != \#* ]] || continue
    name="$(basename "${url%%\?*}")"
    [[ -n "$name" && "$name" != "/" ]] || name="eurlex-dump-$(date +%s)"
    download "$url" "$DEST/dumps/$name"
  done < "$EURLEX_URLS_FILE"
else
  echo "EURLEX_URLS_FILE is not set; wrote source notes only."
fi

if [[ -n "$CELEX_IDS_FILE" ]]; then
  while IFS= read -r celex; do
    [[ -n "$celex" && "$celex" != \#* ]] || continue
    download "https://eur-lex.europa.eu/legal-content/FR/TXT/?uri=CELEX:$celex" \
      "$DEST/celex-fr-html/$celex.html"
  done < "$CELEX_IDS_FILE"
fi

