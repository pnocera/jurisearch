#!/usr/bin/env bash
set -euo pipefail

DEST_ROOT="${DEST_ROOT:-/mnt/models/opendata}"
DEST="$DEST_ROOT/EURLEX"
EURLEX_URLS_FILE="${EURLEX_URLS_FILE:-}"
CELEX_IDS_FILE="${CELEX_IDS_FILE:-}"
EURLEX_CELEX_LANG="${EURLEX_CELEX_LANG:-FRA}"
mkdir -p "$DEST/dumps" "$DEST/celex-$EURLEX_CELEX_LANG-xhtml"

download() {
  local url="$1"
  local out="$2"
  mkdir -p "$(dirname "$out")"
  if [[ -s "$out" ]]; then
    echo "exists: $out"
    return 0
  fi
  echo "download: $url"
  curl -fL --retry 8 --retry-delay 5 --retry-all-errors -C - \
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

download_optional() {
  local url="$1"
  local out="$2"
  mkdir -p "$(dirname "$out")"
  if [[ -s "$out" ]]; then
    echo "exists: $out"
    return 0
  fi
  echo "download: $url"
  if curl -fL --retry 2 --retry-delay 3 -C - \
    -A "jurisearch-dataset-script/1.0" \
    -H "Accept: application/xhtml+xml,text/html,application/xml;q=0.9,*/*;q=0.8" \
    -o "$out.part" "$url"; then
    if [[ -s "$out.part" ]]; then
      mv "$out.part" "$out"
      return 0
    fi
  fi
  rm -f "$out.part"
  echo "failed: $url" >&2
  return 1
}

cat > "$DEST/README.source.txt" <<'EOF'
Source: EUR-Lex reuse services.
No API key is required for public EUR-Lex reuse services, but the full dump URLs
are selected from the EUR-Lex reuse portal rather than a stable one-size script.

Set EURLEX_URLS_FILE to a newline-separated list of official EUR-Lex/Cellar dump
URLs to download them into dumps/.

Optionally set CELEX_IDS_FILE to a newline-separated list of CELEX identifiers.
The script downloads language-specific XHTML through Cellar using the
publications.europa.eu CELEX endpoint. Default language is FRA.

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
  : > "$DEST/celex-failures.tsv"
  while IFS= read -r celex; do
    [[ -n "$celex" && "$celex" != \#* ]] || continue
    if ! download_optional "https://publications.europa.eu/resource/celex/$celex.$EURLEX_CELEX_LANG" \
      "$DEST/celex-$EURLEX_CELEX_LANG-xhtml/$celex.xhtml"; then
      printf '%s\t%s\n' "$celex" "https://publications.europa.eu/resource/celex/$celex.$EURLEX_CELEX_LANG" >> "$DEST/celex-failures.tsv"
    fi
  done < "$CELEX_IDS_FILE"
fi
