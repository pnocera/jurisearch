#!/usr/bin/env python3
"""Minimize over-widened visibility in a split crate's submodules.

  minimize_crate.py <crate-name> <submodule.rs> [<submodule.rs> ...]

Step 1: revert `pub(crate)` -> private in the submodule files (keeps `pub`).
Step 2: loop — `cargo test -p <crate> --no-run`, collect every item the compiler reports
        as not-found / private (E0425/E0432/E0603/E0616/E0624), and bump that item's
        definition (top-level item, struct field, or impl method) to `pub(super)`. Repeat
        until the crate compiles. The result: file-private items stay private; only genuine
        cross-submodule/test items get `pub(super)`.
"""
import re
import subprocess
import sys

crate = sys.argv[1]
files = sys.argv[2:]

TOP = re.compile(r'^pub\(crate\) ')
INDENT = re.compile(r'^(\s+)pub\(crate\) ')


def strip(path):
    out = []
    for l in open(path).read().split('\n'):
        if TOP.match(l):
            out.append(l[len('pub(crate) '):])
        else:
            m = INDENT.match(l)
            out.append(m.group(1) + l[len(m.group(1)) + len('pub(crate) '):] if m else l)
    open(path, 'w').write('\n'.join(out))


def build_errors():
    r = subprocess.run(["cargo", "test", "-p", crate, "--no-run"],
                       capture_output=True, text=True)
    return r.stdout + r.stderr


FIELD_RE = re.compile(r'field `([A-Za-z_][A-Za-z0-9_]*)` of (?:struct|union) `[^`]*?([A-Za-z_][A-Za-z0-9_]*)`')


def collect(out):
    """Return (item_names, field_pairs) — items/methods from generic errors, fields from E0616."""
    items = set()
    fields = set()  # (struct, field)
    for line in out.split('\n'):
        for fm in FIELD_RE.finditer(line):
            fields.add((fm.group(2), fm.group(1)))
        if any(k in line for k in ('cannot find', 'unresolved', 'is private', 'no `')):
            for m in re.findall(r'`([A-Za-z_][A-Za-z0-9_]*)`', line):
                items.add(m)
    return items, fields


ITEM = r'(?:fn|struct|enum|const|static|type)'


def bump(items, fields):
    bumped = set()
    field_structs = {s for s, _ in fields}
    field_names = {f for _, f in fields}
    for path in files:
        lines = open(path).read().split('\n')
        changed = False
        in_struct = None  # struct name whose body we're inside (for fields)
        for i, l in enumerate(lines):
            sm = re.match(rf'^(?:pub(?:\([^)]*\))? )?struct ([A-Za-z_][A-Za-z0-9_]*)', l)
            if sm:
                in_struct = sm.group(1) if sm.group(1) in field_structs else None
            elif l.startswith('}'):
                in_struct = None
            if l.lstrip().startswith('pub'):
                continue
            # struct-definition field, only when flagged via E0616 and inside the right struct
            if in_struct is not None:
                fm = re.match(r'^(\s+)([a-z_][A-Za-z0-9_]*)\s*:', l)
                if fm and fm.group(2) in field_names:
                    lines[i] = fm.group(1) + 'pub(super) ' + l[len(fm.group(1)):]
                    changed = True; bumped.add('field:' + fm.group(2)); continue
            # top-level item
            m = re.match(rf'^({ITEM}) ([A-Za-z_][A-Za-z0-9_]*)', l)
            if m and m.group(2) in items:
                lines[i] = 'pub(super) ' + l; changed = True; bumped.add(m.group(2)); continue
            # indented method
            m = re.match(rf'^(\s+)((?:async\s+|unsafe\s+)*fn) ([A-Za-z_][A-Za-z0-9_]*)', l)
            if m and m.group(3) in items:
                lines[i] = m.group(1) + 'pub(super) ' + l[len(m.group(1)):]
                changed = True; bumped.add(m.group(3)); continue
        if changed:
            open(path, 'w').write('\n'.join(lines))
    return bumped


for p in files:
    strip(p)
for rounds in range(15):
    out = build_errors()
    if 'error[' not in out and 'error:' not in out:
        print(f"clean after {rounds} bump round(s)")
        break
    items, fields = collect(out)
    bumped = bump(items, fields)
    if not bumped:
        print("STUCK — remaining errors not auto-bumpable:")
        print('\n'.join(l for l in out.split('\n') if l.startswith('error'))[:2000])
        sys.exit(1)
    print(f"round {rounds}: bumped {sorted(bumped)}")
else:
    print("did not converge")
    sys.exit(1)
