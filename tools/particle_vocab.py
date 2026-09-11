#!/usr/bin/env python3
"""Inventory every emitter / initializer / operator / renderer name and the
parameters each carries, across the whole particle corpus.

This is the surface a simulator must implement. If a name appears here some
wallpaper needs it; the parameter union tells us what each one means.
"""
import glob, json, os, sys, collections
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pkgread import Pkg

root = os.path.expanduser('~/.steam/root/steamapps/workshop/content/431960')

params = collections.defaultdict(lambda: collections.defaultdict(collections.Counter))
counts = collections.defaultdict(collections.Counter)
materials = collections.Counter()
renderer_names = collections.Counter()
visited = set()
defs_seen = 0
maxcounts = []
starttimes = []

for d in sorted(glob.glob(root + '/*')):
    sj = os.path.join(d, 'scene.pkg')
    if not os.path.exists(sj):
        continue
    try:
        pkg = Pkg(sj)
        scene = json.loads(pkg.get('scene.json') or b'{}')
    except Exception:
        continue

    def walk(name, depth=0):
        global defs_seen
        if not name or depth > 8 or name in visited:
            return
        visited.add(name)
        body = pkg.get(name)
        if not body:
            return
        try:
            pj = json.loads(body)
        except Exception:
            return
        defs_seen += 1
        for section in ('emitter', 'initializer', 'operator', 'renderer'):
            for item in (pj.get(section) or []):
                nm = item.get('name')
                if not nm:
                    continue
                counts[section][nm] += 1
                for k in item:
                    if k not in ('id', 'name'):
                        params[section][nm][k] += 1
        if pj.get('material'):
            materials[pj['material']] += 1
        if isinstance(pj.get('maxcount'), (int, float)):
            maxcounts.append(pj['maxcount'])
        if isinstance(pj.get('starttime'), (int, float)):
            starttimes.append(pj['starttime'])
        for ch in (pj.get('children') or []):
            walk(ch.get('name'), depth + 1)

    for o in scene.get('objects', []):
        walk(o.get('particle'))
        # some scenes nest particles under children
        for ch in (o.get('children') or []):
            if isinstance(ch, dict):
                walk(ch.get('particle'))

print(f'particle definition files parsed: {defs_seen}')
for section in ('emitter', 'initializer', 'operator', 'renderer'):
    print(f'\n===== {section} ({len(counts[section])} distinct) =====')
    for nm, c in counts[section].most_common():
        keys = ','.join(sorted(params[section][nm].keys()))
        print(f'  {c:4}  {nm:<34} [{keys}]')

print(f'\nmaxcount: {len(maxcounts)} defs, min {min(maxcounts) if maxcounts else "-"}, max {max(maxcounts) if maxcounts else "-"}')
print(f'starttime: {len(starttimes)} defs, range {min(starttimes) if starttimes else "-"}..{max(starttimes) if starttimes else "-"}')
print('\n===== top materials =====')
for m, c in materials.most_common(12):
    print(f'  {c:4}  {m}')
