"""Classify the identifier substitutions in a unified diff of decompiled output.

usage: python -I substitutions.py <diff> [top-N]

Pairs each hunk's removed and added lines one to one (hunks with equal counts
only) and, where two lines tokenise to the same length, records every token
that changed as `old -> new`. Substitutions are classed by whether each side is
a decompiler placeholder (`closure_N`, `closure_N$K`, `closure_L_S`, `cN`):

  placeholder -> name    a placeholder got a name (usually the goal)
  name -> placeholder    a name was lost
  name -> name           a name changed: the class to read, the top-N of it
                         are printed with counts

Lines that do not pair up are counted, not classified; a large count means the
diff is structural rather than a renaming, and needs reading.
"""

import re
import sys
from collections import Counter

TOKEN = re.compile(r"[A-Za-z_$][\w$]*|\S")
PLACEHOLDER = re.compile(r"^(closure_\d+(?:_\d+|\$\d+)?|c\d+)$")


def hunks(lines):
    current = None
    for line in lines:
        if line.startswith("@@"):
            if current:
                yield current
            current = ([], [])
        elif current is not None and line.startswith("-") and not line.startswith("---"):
            current[0].append(line[1:])
        elif current is not None and line.startswith("+") and not line.startswith("+++"):
            current[1].append(line[1:])
    if current:
        yield current


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    lines = open(sys.argv[1], encoding="utf-8", errors="replace").read().splitlines()
    top = int(sys.argv[2]) if len(sys.argv) > 2 else 25
    subs = Counter()
    unpaired = 0
    for old, new in hunks(lines):
        if len(old) != len(new):
            unpaired += 1
            continue
        for a, b in zip(old, new):
            ta, tb = TOKEN.findall(a), TOKEN.findall(b)
            if len(ta) != len(tb):
                unpaired += 1
                continue
            subs.update((x, y) for x, y in zip(ta, tb) if x != y)
    classes = Counter()
    named = Counter()
    for (x, y), n in subs.items():
        px, py = bool(PLACEHOLDER.match(x)), bool(PLACEHOLDER.match(y))
        if px and not py:
            classes["placeholder -> name"] += n
        elif py and not px:
            classes["name -> placeholder"] += n
        elif px and py:
            classes["placeholder -> placeholder"] += n
        else:
            classes["name -> name"] += n
            named[(x, y)] += n
    print(dict(classes), "unpaired:", unpaired)
    for (x, y), n in named.most_common(top):
        print(f"{n:6d}  {x} -> {y}")


main()
