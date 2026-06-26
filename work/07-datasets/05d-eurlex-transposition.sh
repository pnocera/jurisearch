#!/usr/bin/env bash
set -euo pipefail

DEST_ROOT="${DEST_ROOT:-/mnt/models/opendata}"
DEST="$DEST_ROOT/EURLEX"
CELEX_IDS_FILE="${CELEX_IDS_FILE:-work/07-datasets/eurlex-business-celex.txt}"
EURLEX_NIM_COUNTRY_FILTER="${EURLEX_NIM_COUNTRY_FILTER:-FRA}"
mkdir -p "$DEST/work-rdf" "$DEST/transposition"

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
    -H "Accept: application/rdf+xml,application/xml;q=0.9,*/*;q=0.8" \
    -o "$out.part" "$url"
  if [[ ! -s "$out.part" ]]; then
    rm -f "$out.part"
    echo "empty download: $url" >&2
    return 1
  fi
  mv "$out.part" "$out"
}

while IFS= read -r celex; do
  [[ -n "$celex" && "$celex" != \#* ]] || continue
  [[ "$celex" == 3*L* ]] || continue
  download "https://publications.europa.eu/resource/celex/$celex" "$DEST/work-rdf/$celex.rdf"
done < "$CELEX_IDS_FILE"

python3 - "$DEST" "$CELEX_IDS_FILE" "$EURLEX_NIM_COUNTRY_FILTER" <<'PY'
import os
import re
import sys
import xml.etree.ElementTree as ET

dest, celex_file, country = sys.argv[1], sys.argv[2], sys.argv[3]
RDF_RESOURCE = "{http://www.w3.org/1999/02/22-rdf-syntax-ns#}resource"
RDF_ABOUT = "{http://www.w3.org/1999/02/22-rdf-syntax-ns#}about"

def local(tag): return tag.rsplit("}", 1)[-1]
def clean(text): return (text or "").strip().replace("\t", " ").replace("\n", " ")
def rid(uri): return (uri or "").rstrip("/").rsplit("/", 1)[-1]

os.makedirs(os.path.join(dest, "transposition"), exist_ok=True)
out = open(os.path.join(dest, "transposition", "national-transposition.tsv"), "w", encoding="utf-8")
out.write("directive_celex\trelation\tnim_id_or_uri\n")
dates = open(os.path.join(dest, "transposition", "directive-transposition-dates.tsv"), "w", encoding="utf-8")
dates.write("directive_celex\tfield\tvalue\n")
seen_nims = set()
seen_dates = set()

for line in open(celex_file, encoding="utf-8"):
    celex = line.strip()
    if not celex or celex.startswith("#") or not celex.startswith("3") or "L" not in celex:
        continue
    path = os.path.join(dest, "work-rdf", f"{celex}.rdf")
    if not os.path.exists(path):
        continue
    root = ET.parse(path).getroot()
    country_nims = set()
    if country:
        for desc in root.iter():
            if local(desc.tag) != "Description" or rid(desc.attrib.get(RDF_ABOUT, "")) != country:
                continue
            for child in list(desc):
                name = local(child.tag)
                target = child.attrib.get(RDF_RESOURCE)
                if "country_implements" in name and target and "/resource/nim/" in target:
                    country_nims.add(rid(target))

    for desc in root.iter():
        if local(desc.tag) != "Description" or rid(desc.attrib.get(RDF_ABOUT, "")) != celex:
            continue
        for elem in list(desc):
            name = local(elem.tag)
            target = elem.attrib.get(RDF_RESOURCE)
            value = clean(elem.text)
            if name in {"date_transposition", "directive_date_transposition"} and value:
                row = (celex, name, value)
                if row not in seen_dates:
                    seen_dates.add(row)
                    dates.write("\t".join(row) + "\n")
            if "implement" in name and target and "/resource/nim/" in target:
                nim = rid(target)
                if country and country_nims and nim not in country_nims:
                    continue
                row = (celex, name, nim)
                if row not in seen_nims:
                    seen_nims.add(row)
                    out.write("\t".join(row) + "\n")

out.close()
dates.close()
PY

cat > "$DEST/transposition/README.txt" <<EOF
National implementation/transposition metadata extracted from Cellar RDF.

national-transposition.tsv columns:
directive_celex, relation, nim_id_or_uri

directive-transposition-dates.tsv columns:
directive_celex, field, value

EURLEX_NIM_COUNTRY_FILTER=$EURLEX_NIM_COUNTRY_FILTER filters NIM ids through
country_implements_measure_national_implementing links when the country node is
present in the directive RDF. Set it to an empty string to keep all NIM ids
exposed by the directive RDF.
EOF
