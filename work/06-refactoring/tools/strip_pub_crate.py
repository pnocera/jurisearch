#!/usr/bin/env python3
"""Revert `pub(crate)` -> private in the given split-submodule files (NOT the hub/lib/tests).
After stripping, build the crate and re-add `pub(super)` only for the items the compiler
flags as needed cross-submodule (E0603). Leaves `pub` (public API) untouched.
"""
import sys
import re

TOP = re.compile(r'^pub\(crate\) ')
INDENT = re.compile(r'^(\s+)pub\(crate\) ')

for path in sys.argv[1:]:
    lines = open(path).read().split('\n')
    out = []
    n = 0
    for l in lines:
        if TOP.match(l):
            out.append(l[len('pub(crate) '):])
            n += 1
        else:
            m = INDENT.match(l)
            if m:
                out.append(m.group(1) + l[len(m.group(1)) + len('pub(crate) '):])
                n += 1
            else:
                out.append(l)
    open(path, 'w').write('\n'.join(out))
    print(f"{path}: stripped {n}")
