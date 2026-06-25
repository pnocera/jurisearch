#!/usr/bin/env python3
"""One-off: split jurisearch-core schema.rs into schema/{mod,search,admin,eval,gates}.rs.

compiled_schema() is a single json!({...}) tree whose per-command schemas all nest under one
flat "schemas" key. serde_json's Value is a sorted BTreeMap (no preserve_order feature), so the
emitted JSON is key-sorted regardless of merge order — we can regroup the entries freely and
re-merge in mod.rs. A golden byte-identical test guards the result.
"""
import re
import os

SRC = "crates/jurisearch-core/src/schema.rs"
OUTDIR = "crates/jurisearch-core/src/schema"

SEARCH = {"SearchRequest", "SearchResponse", "CompareRequest", "CompareResponse",
          "FetchRequest", "FetchResponse", "CiteRequest", "CiteResponse",
          "ContextRequest", "ContextResponse", "ExpandRequest", "ExpandResponse",
          "RelatedRequest", "RelatedResponse"}
EVAL = {"EvalPhase1Request", "EvalPhase1Response", "EvalFranceLegiRequest",
        "EvalFranceLegiResponse", "EvalRunRequest", "EvalRunResponse", "EvalTuneRequest",
        "EvalTuneResponse", "EvalFixtureSummary", "FranceLegiCategory"}
GATES = {"Phase2GateResponse", "Phase2BenchmarkGate", "Phase1GateResponse", "Phase1GateCheck",
         "ExternalBenchmarkGate", "FranceLegiGate", "RerankerDecision"}


def domain(key):
    if key in SEARCH:
        return "search"
    if key in EVAL:
        return "eval"
    if key in GATES:
        return "gates"
    return "admin"


def main():
    lines = open(SRC).read().split('\n')
    # locate the "schemas": { ... } block (8-space indented key).
    start = next(i for i, l in enumerate(lines) if l.startswith('        "schemas": {'))
    # matching close: first line that is exactly 8-space '}' after start.
    end = next(i for i in range(start + 1, len(lines)) if lines[i] == '        }')
    body = lines[start + 1:end]          # entry lines (12-space keys)
    head = lines[:start]                 # up to and incl. the common keys + `"schemas": {` is at start
    # `head` currently ends just before the `"schemas": {` line; we drop that line and the block.
    # Everything after the block close `}` (line `end`) — the `})` + fn close + tests.
    tail = lines[end + 1:]

    # parse entries: each starts at a 12-space `"Key":` line.
    entry_re = re.compile(r'^            "([A-Za-z][A-Za-z0-9]*)":')
    starts = [i for i, l in enumerate(body) if entry_re.match(l)]
    groups = {"search": [], "admin": [], "eval": [], "gates": []}
    for idx, s in enumerate(starts):
        e = starts[idx + 1] if idx + 1 < len(starts) else len(body)
        text = body[s:e]
        # strip trailing blank lines; ensure the entry ends with a comma for safe re-join.
        while text and text[-1].strip() == '':
            text.pop()
        if text and not text[-1].rstrip().endswith(','):
            text[-1] = text[-1].rstrip() + ','
        key = entry_re.match(body[s]).group(1)
        groups[domain(key)].append('\n'.join(text))

    os.makedirs(OUTDIR, exist_ok=True)
    for dom, entries in groups.items():
        # entries are 12-space-indented; reindent to 8-space inside `json!({ ... })`.
        joined = '\n'.join(entries)
        reindented = '\n'.join(l[4:] if l.startswith('    ') else l for l in joined.split('\n'))
        doc = {
            "search": "Search-family command schemas (search/compare/fetch/cite/context/related/expand).",
            "admin": "Admin/introspection command schemas (status/model/setup/doctor/stats/inspect/versions/diff/sync/help/serve/ingest/session).",
            "eval": "Eval/benchmark command schemas (phase1/run/tune/France-LEGI/France-juris).",
            "gates": "Release-gate schemas (Phase 1/Phase 2 gate + benchmark-gate support).",
        }[dom]
        with open(f"{OUTDIR}/{dom}.rs", 'w') as f:
            f.write(f"//! {doc}\n//!\n//! Returns this domain's entries for the flat `#/schemas/*` map "
                    f"assembled by `compiled_schema()`.\n\n")
            f.write("use serde_json::{Map, Value, json};\n\n")
            f.write("pub(crate) fn schemas() -> Map<String, Value> {\n")
            f.write("    let Value::Object(map) = json!({\n")
            f.write(reindented + "\n")
            f.write("    }) else {\n        unreachable!()\n    };\n    map\n}\n")

    # build mod.rs: head (up to but not incl `"schemas": {`) + merged schemas + tail.
    # head's last non-empty content is `"common_enums": { ... },`. We append the schemas assembly.
    modrs = []
    modrs.append("use serde_json::{Map, Value, json};")
    modrs.append("")
    modrs.append("use crate::{SCHEMA_VERSION, contract::COMMANDS};")
    modrs.append("")
    modrs.append("mod admin;")
    modrs.append("mod eval;")
    modrs.append("mod gates;")
    modrs.append("mod search;")
    modrs.append("")
    modrs.append("pub fn compiled_schema() -> Value {")
    modrs.append("    let mut schemas: Map<String, Value> = Map::new();")
    modrs.append("    schemas.extend(search::schemas());")
    modrs.append("    schemas.extend(admin::schemas());")
    modrs.append("    schemas.extend(eval::schemas());")
    modrs.append("    schemas.extend(gates::schemas());")
    # the json!({...}) with common keys, ending with `"schemas": Value::Object(schemas)`.
    # head lines from `pub fn compiled_schema() -> Value {` (orig) up to the line before `"schemas": {`.
    # We want the inner json!({ ...common keys... }) lines. Find them in `head`.
    fn_start = next(i for i, l in enumerate(head) if l.startswith('pub fn compiled_schema'))
    json_open = next(i for i in range(fn_start, len(head)) if head[i].strip() == 'json!({')
    common = head[json_open + 1:start]  # common key lines (8-space), excludes `"schemas": {`
    modrs.append("    json!({")
    modrs.extend(common)
    modrs.append('        "schemas": Value::Object(schemas)')
    modrs.append("    })")
    modrs.append("}")
    # tail begins after the schemas-close `}`; the original next lines are `    })` `}` then tests.
    # We've already emitted the fn body close, so skip the original `    })` and `}` (first two
    # non-blank tail lines) and keep from the test module onward.
    t = 0
    skipped = 0
    while t < len(tail) and skipped < 2:
        s = tail[t].strip()
        if s in (')', '})', '}'):
            skipped += 1
            t += 1
            continue
        if s == '':
            t += 1
            continue
        break
    modrs.append("")
    modrs.extend(tail[t:])
    open(f"{OUTDIR}/mod.rs", 'w').write('\n'.join(modrs))
    os.remove(SRC)
    print(f"wrote schema/ with search={len(groups['search'])} admin={len(groups['admin'])} "
          f"eval={len(groups['eval'])} gates={len(groups['gates'])} entries")


if __name__ == '__main__':
    main()
