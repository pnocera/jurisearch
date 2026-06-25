#!/usr/bin/env python3
"""Move a file's trailing `#[cfg(test)] mod tests { ... }` into a sibling tests.rs,
replacing it with `#[cfg(test)] mod tests;`. Dedents the body one level but is
raw-string-aware: lines inside an `r#"..."#` literal are left byte-identical, so XML/JSON
fixtures are preserved exactly.

Usage: move_test_module.py <src.rs> <out_tests.rs> "<doc line>"
"""
import sys


def main():
    src, out, doc = sys.argv[1], sys.argv[2], sys.argv[3]
    lines = open(src).read().split('\n')
    start = None
    for i, l in enumerate(lines):
        if l.strip() == '#[cfg(test)]' and i + 1 < len(lines) and lines[i + 1].strip() == 'mod tests {':
            start = i
            break
    if start is None:
        print("no trailing '#[cfg(test)] mod tests {' found", file=sys.stderr)
        sys.exit(1)
    op = start + 1
    end = None
    for j in range(op + 1, len(lines)):
        if lines[j].startswith('}'):
            end = j
            break
    if end is None:
        print("no closing brace at column 0", file=sys.stderr)
        sys.exit(1)
    body = lines[op + 1:end]
    deds = []
    in_raw = False
    for line in body:
        if in_raw:
            deds.append(line)
            if '"#' in line:
                in_raw = False
            continue
        if line == '':
            deds.append('')
        elif line.startswith('    '):
            deds.append(line[4:])
        else:
            deds.append(line)
        if 'r#"' in line and '"#' not in line.split('r#"', 1)[1]:
            in_raw = True
    open(out, 'w').write(f"//! {doc}\n\n" + '\n'.join(deds) + '\n')
    new = lines[:start] + ['#[cfg(test)]', 'mod tests;'] + lines[end + 1:]
    open(src, 'w').write('\n'.join(new))
    print(f"moved {end - op - 1} body lines; {src} now {len(new)}; {out} {len(deds)+2}")


if __name__ == '__main__':
    main()
