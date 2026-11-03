#!/usr/bin/env python3
"""Diff two computed-style dumps (dark.json vs light.json from render_prejs_dump.py) and print
every element whose bg / bgi / border / colour / shadow changes between the two themes.
Usage: python3 theme_diff.py dark.json light.json"""
import json, sys
d = json.load(open(sys.argv[1]))['items']
l = json.load(open(sys.argv[2]))['items']
ld = {}
for it in l:
    ld.setdefault((it['s'], it['txt'], it['x'], it['y']), it)
seen = set()
for it in d:
    o = ld.get((it['s'], it['txt'], it['x'], it['y']))
    if not o:
        continue
    diffs = [f"{f}: {it[f][:70]} -> {o[f][:70]}" for f in ('bg', 'bgi', 'bd', 'col', 'sh') if it[f] != o[f]]
    if diffs:
        key = (it['s'], tuple(diffs))
        if key in seen:
            continue
        seen.add(key)
        print(f"[{it['s'][:60]}] '{it['txt'][:20]}' {it['w']}x{it['h']}")
        for x in diffs:
            print("    ", x)
