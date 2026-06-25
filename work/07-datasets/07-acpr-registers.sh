#!/usr/bin/env bash
set -euo pipefail

DEST_ROOT="${DEST_ROOT:-/mnt/models/opendata}"
DEST="$DEST_ROOT/ACPR"
PAGE_URL="https://acpr.banque-france.fr/fr/professionnels/vos-outils-et-services/consulter-les-registres/registre-des-agents-financiers-et-des-organismes-dassurance"
mkdir -p "$DEST/refassu"

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

download "$PAGE_URL" "$DEST/acpr-registers-page.html"

python3 - "$DEST/acpr-registers-page.html" > "$DEST/refassu-manifest.tsv" <<'PY'
import html
import re
import sys
from urllib.parse import urljoin

page_url = "https://acpr.banque-france.fr/fr/professionnels/vos-outils-et-services/consulter-les-registres/registre-des-agents-financiers-et-des-organismes-dassurance"
text = open(sys.argv[1], "r", encoding="utf-8", errors="ignore").read()
seen = set()
for href in re.findall(r'href\s*=\s*["\']([^"\']+\.xlsx(?:\?[^"\']*)?)["\']', text, flags=re.I):
    url = urljoin(page_url, html.unescape(href))
    if url in seen:
        continue
    seen.add(url)
    name = re.sub(r"[^A-Za-z0-9._-]+", "-", url.rsplit("/", 1)[-1].split("?", 1)[0]).strip("-")
    print(f"{url}\t{name}")
PY

while IFS=$'\t' read -r url name; do
  [[ -n "${url:-}" && -n "${name:-}" ]] || continue
  download "$url" "$DEST/refassu/$name"
done < "$DEST/refassu-manifest.tsv"

cat > "$DEST/README.source.txt" <<'EOF'
Source: ACPR public registers page.
The REFASSU XLSX files linked from the public ACPR page do not require an API key.

REGAFI also exposes a developer API, but access appears to require portal
registration/subscription. Configure that separately before adding automated
REGAFI API extraction.

REGAFI developer portal:
https://developer.regafi.banque-france.fr/
EOF
