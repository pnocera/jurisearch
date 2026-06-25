#!/usr/bin/env python3
"""Mechanical Rust top-level item mover for the jurisearch-cli refactor.

Usage:
  cli_split.py spans <srcfile>
      Print every top-level item: anchor(1-based) header_start..end name/kind.
  cli_split.py show <srcfile> <anchor1,anchor2,...>
      Dry-run: print computed span for each requested anchor line (1-based).
  cli_split.py extract <srcfile> <out_blob> <anchor1,anchor2,...>
      Remove the items at the given anchor lines from <srcfile> (in place) and
      write their verbatim text (in source order) to <out_blob>.

Items are matched by the 1-based line number of their *anchor* (the `fn`/`struct`/
`enum`/`impl`/... line at column 0), which is unambiguous even for impl blocks.
The span includes contiguous leading doc-comments / attributes / `//` comments.
"""
import re
import sys

ITEM_RE = re.compile(
    r'^(pub(\(crate\))?(\s+)|pub\s+)?'
    r'(async\s+)?(unsafe\s+)?'
    r'(fn|struct|enum|impl|trait|const|static|type|mod|macro_rules!)\b'
)
NAME_RE = re.compile(
    r'(?:fn|struct|enum|trait|const|static|type|mod)\s+([A-Za-z_][A-Za-z0-9_]*)'
)
KIND_RE = re.compile(
    r'^(?:pub(?:\([^)]*\))?\s+)?'
    r'(?:async\s+)?(?:unsafe\s+)?'
    r'(fn|struct|enum|impl|trait|const|static|type|mod|macro_rules!)\b'
)
FIELD_RE = re.compile(r'^( {4})(r#)?[a-z_][A-Za-z0-9_]*\s*:')
METHOD_RE = re.compile(r'^( {4})((?:async\s+)?(?:unsafe\s+)?fn)\b')


def kind_of(line):
    m = KIND_RE.match(line)
    return m.group(1) if m else '?'


def _already_pub(s):
    # True only for a visibility modifier `pub`, `pub `, `pub(...)` — NOT an
    # identifier that merely starts with "pub" (e.g. a field named `publication`).
    return s == 'pub' or s.startswith('pub ') or s.startswith('pub(')


def ensure_pub(line):
    if _already_pub(line.lstrip()):
        return line
    return 'pub(crate) ' + line


def pub_indent(line, group_keyword):
    # add pub(crate) to an indented field/method, preserving indent
    stripped = line[4:]
    if _already_pub(stripped):
        return line
    return '    pub(crate) ' + stripped


def is_anchor(line):
    return bool(ITEM_RE.match(line))


def header_start(lines, a):
    """Walk back over contiguous doc/attr/comment lines (stop at blank/other)."""
    hs = a
    j = a - 1
    while j >= 0:
        raw = lines[j]
        stripped = raw.strip()
        if stripped == '':
            break
        ls = raw.lstrip()
        if (ls.startswith('///') or ls.startswith('//!') or ls.startswith('//')
                or raw.startswith('#[') or raw.startswith('#![')):
            hs = j
            j -= 1
        else:
            break
    return hs


def item_end(lines, a):
    """Return 0-based inclusive end line for the item whose anchor is at line a."""
    n = len(lines)
    # Find the line that opens the body ('{') or terminates a statement (';').
    k = a
    while k < n:
        line = lines[k]
        if '{' in line:
            break
        if line.rstrip().endswith(';'):
            return k  # statement item (const/static/type/unit-struct/tuple-struct)
        k += 1
    if k >= n:
        return n - 1
    # lines[k] contains the first '{'.
    if line.count('{') > 0 and line.count('{') == line.count('}'):
        return k  # one-line brace body
    # Multi-line brace item: closing brace sits at column 0.
    k2 = k + 1
    while k2 < n:
        if lines[k2].startswith('}'):
            return k2
        k2 += 1
    return n - 1


def name_of(line):
    m = NAME_RE.search(line)
    if m:
        return m.group(1)
    if line.lstrip().startswith('impl'):
        return line.strip().rstrip('{').strip()
    if 'macro_rules!' in line:
        return line.split('macro_rules!')[1].strip().rstrip('{').strip()
    return '?'


def all_items(lines):
    items = []
    for i, line in enumerate(lines):
        if is_anchor(line):
            hs = header_start(lines, i)
            end = item_end(lines, i)
            items.append({'anchor': i, 'hs': hs, 'end': end,
                          'name': name_of(line), 'kind': kind_of(line)})
    return items


def pubify_file(path):
    with open(path) as f:
        lines = f.read().split('\n')
    items = all_items(lines)
    changes = 0
    for it in items:
        a, end, kind = it['anchor'], it['end'], it['kind']
        if kind in ('fn', 'struct', 'enum', 'type', 'const', 'static'):
            new = ensure_pub(lines[a])
            if new != lines[a]:
                lines[a] = new
                changes += 1
        if kind == 'struct':
            for k in range(a + 1, end):
                if FIELD_RE.match(lines[k]):
                    new = pub_indent(lines[k], None)
                    if new != lines[k]:
                        lines[k] = new
                        changes += 1
        if kind == 'impl' and ' for ' not in lines[a]:
            for k in range(a + 1, end):
                if METHOD_RE.match(lines[k]):
                    new = pub_indent(lines[k], None)
                    if new != lines[k]:
                        lines[k] = new
                        changes += 1
    with open(path, 'w') as f:
        f.write('\n'.join(lines))
    print(f"pubified {path}: {changes} lines changed")


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        sys.exit(2)
    mode = sys.argv[1]
    src = sys.argv[2]
    with open(src) as f:
        lines = f.read().split('\n')
    # note: split keeps a trailing '' if file ends with newline.

    if mode == 'spans':
        for it in all_items(lines):
            print(f"{it['anchor']+1}\t{it['hs']+1}..{it['end']+1}\t{it['kind']}\t{it['name']}")
        return

    if mode == 'pubify':
        pubify_file(src)
        return

    if mode == 'names':
        # Resolve a comma-separated list of item names to their 1-based anchor lines.
        # Names match the `name` field from `spans` (impls: e.g. "impl Foo" / "impl A for B").
        wanted = [n.strip() for n in sys.argv[-1].split(',') if n.strip()]
        items = all_items(lines)
        by_name = {}
        for it in items:
            by_name.setdefault(it['name'], []).append(it['anchor'] + 1)
        out = []
        for n in wanted:
            hits = by_name.get(n, [])
            if len(hits) == 0:
                print(f"ERROR: name not found: {n!r}", file=sys.stderr)
                sys.exit(5)
            if len(hits) > 1:
                print(f"ERROR: ambiguous name {n!r} -> anchors {hits}", file=sys.stderr)
                sys.exit(6)
            out.append(str(hits[0]))
        print(','.join(out))
        return

    anchors_arg = sys.argv[-1]
    want = [int(x) - 1 for x in anchors_arg.split(',') if x.strip()]
    items = all_items(lines)
    by_anchor = {it['anchor']: it for it in items}
    chosen = []
    for w in want:
        if w not in by_anchor:
            print(f"ERROR: no top-level item anchored at line {w+1}", file=sys.stderr)
            sys.exit(3)
        chosen.append(by_anchor[w])
    chosen.sort(key=lambda it: it['hs'])
    # overlap check
    for p, q in zip(chosen, chosen[1:]):
        if p['end'] >= q['hs']:
            print(f"ERROR: overlap {p['name']} and {q['name']}", file=sys.stderr)
            sys.exit(4)

    if mode == 'show':
        for it in chosen:
            print(f"--- {it['name']} (anchor {it['anchor']+1}) span "
                  f"{it['hs']+1}..{it['end']+1} "
                  f"[{it['end']-it['hs']+1} lines]")
            print(f"    first: {lines[it['hs']]!r}")
            print(f"    last : {lines[it['end']]!r}")
        return

    if mode == 'extract':
        out_blob = sys.argv[3]
        # Build extracted text in source order.
        blocks = []
        for it in chosen:
            blocks.append('\n'.join(lines[it['hs']:it['end'] + 1]))
        with open(out_blob, 'w') as f:
            f.write('\n\n'.join(blocks) + '\n')
        # Remove from source, bottom-to-top.
        keep = list(lines)
        for it in sorted(chosen, key=lambda x: x['hs'], reverse=True):
            # also swallow a single trailing blank line after the item, if present,
            # to avoid piling up blank lines.
            end = it['end']
            if end + 1 < len(keep) and keep[end + 1].strip() == '':
                end += 1
            del keep[it['hs']:end + 1]
        with open(src, 'w') as f:
            f.write('\n'.join(keep))
        print(f"extracted {len(chosen)} items -> {out_blob}; "
              f"src now {len(keep)} lines (was {len(lines)})")
        return

    print(f"unknown mode {mode}", file=sys.stderr)
    sys.exit(2)


if __name__ == '__main__':
    main()
