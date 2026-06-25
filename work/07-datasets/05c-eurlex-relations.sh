#!/usr/bin/env bash
set -euo pipefail

DEST_ROOT="${DEST_ROOT:-/mnt/models/opendata}"
DEST="$DEST_ROOT/EURLEX"
CELEX_IDS_FILE="${CELEX_IDS_FILE:-work/07-datasets/eurlex-business-celex.txt}"
EURLEX_CELEX_LANG="${EURLEX_CELEX_LANG:-FRA}"
mkdir -p "$DEST/work-rdf" "$DEST/expression-rdf" "$DEST/relations"

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
  download "https://publications.europa.eu/resource/celex/$celex" "$DEST/work-rdf/$celex.rdf"
  download "https://publications.europa.eu/resource/celex/$celex.$EURLEX_CELEX_LANG" "$DEST/expression-rdf/$celex.$EURLEX_CELEX_LANG.rdf" || true
done < "$CELEX_IDS_FILE"

python3 - "$DEST" "$CELEX_IDS_FILE" "$EURLEX_CELEX_LANG" <<'PY'
import os
import re
import sys
import xml.etree.ElementTree as ET

dest, celex_file, lang = sys.argv[1], sys.argv[2], sys.argv[3]
RDF_RESOURCE = "{http://www.w3.org/1999/02/22-rdf-syntax-ns#}resource"
RDF_ABOUT = "{http://www.w3.org/1999/02/22-rdf-syntax-ns#}about"

def local(tag: str) -> str:
    return tag.rsplit("}", 1)[-1]

def rid(uri: str) -> str:
    return (uri or "").rstrip("/").rsplit("/", 1)[-1]

def parse(path):
    if not os.path.exists(path) or os.path.getsize(path) == 0:
        return None
    return ET.parse(path).getroot()

def celex_ids():
    for line in open(celex_file, encoding="utf-8"):
        line = line.strip()
        if line and not line.startswith("#"):
            yield line

os.makedirs(os.path.join(dest, "relations"), exist_ok=True)
meta = open(os.path.join(dest, "relations", "work-metadata.tsv"), "w", encoding="utf-8")
cases = open(os.path.join(dest, "relations", "case-law-links.tsv"), "w", encoding="utf-8")
cit = open(os.path.join(dest, "relations", "work-citations.tsv"), "w", encoding="utf-8")
amend = open(os.path.join(dest, "relations", "amendment-links.tsv"), "w", encoding="utf-8")
subjects = open(os.path.join(dest, "relations", "eurovoc-subjects.tsv"), "w", encoding="utf-8")

meta.write("celex\tfield\tvalue\n")
cases.write("source_celex\trelation\ttarget_celex_or_uri\n")
cit.write("source_celex\trelation\ttarget_celex_or_uri\n")
amend.write("source_celex\trelation\ttarget_celex_or_uri\n")
subjects.write("source_celex\teurovoc_uri\n")

for celex in celex_ids():
    for kind, path in (
        ("work", os.path.join(dest, "work-rdf", f"{celex}.rdf")),
        ("expression", os.path.join(dest, "expression-rdf", f"{celex}.{lang}.rdf")),
    ):
        root = parse(path)
        if root is None:
            continue
        for desc in root.iter():
            if local(desc.tag) != "Description":
                continue
            about = desc.attrib.get(RDF_ABOUT, "")
            about_id = rid(about)
            if about_id not in {celex, f"{celex}.{lang}"} and not about_id.startswith("JOL_"):
                pass
            for child in list(desc):
                name = local(child.tag)
                if name.startswith("annotated"):
                    continue
                target = child.attrib.get(RDF_RESOURCE)
                text = (child.text or "").strip().replace("\t", " ").replace("\n", " ")
                if name in {"title", "expression_title", "title_short", "expression_title_short", "date_document", "work_date_document"} and text:
                    meta.write(f"{celex}\t{name}\t{text}\n")
                if target:
                    target_id = rid(target)
                    if "interpreted_by_case" in name or re.match(r"6[0-9].*", target_id):
                        cases.write(f"{celex}\t{name}\t{target_id or target}\n")
                    elif "cited_by" in name or "cites" in name:
                        cit.write(f"{celex}\t{name}\t{target_id or target}\n")
                    elif any(word in name for word in ("amend", "correct", "repeal", "based_on")):
                        amend.write(f"{celex}\t{name}\t{target_id or target}\n")
                    elif "eurovoc" in name or "subject" in name and "eurovoc.europa.eu" in target:
                        subjects.write(f"{celex}\t{target}\n")

for f in (meta, cases, cit, amend, subjects):
    f.close()
PY

cat > "$DEST/relations/README.txt" <<EOF
Compact Cellar RDF extraction for CELEX ids in $CELEX_IDS_FILE.

Files:
- work-metadata.tsv: title/date/title_short values found on work/expression RDF.
- case-law-links.tsv: CJEU/case-law interpretation and case CELEX links.
- work-citations.tsv: work_cited_by_work/citation-style relations.
- amendment-links.tsv: amendment/correction/repeal/basis relations.
- eurovoc-subjects.tsv: EuroVoc subject URIs where exposed.
EOF
