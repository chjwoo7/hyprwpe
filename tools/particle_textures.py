#!/usr/bin/env python3
"""How many particle sprite textures resolve inside their own package?

A particle material names a texture like "particle/halo". Wallpaper Engine ships
a built-in asset library with those names, so a package only carries the ones a
creator overrode. This measures the split, which decides whether a renderer needs
the built-in assets or can rely on the package alone.
"""
import glob, json, os, sys, collections
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pkgread import Pkg

root = os.path.expanduser('~/.steam/root/steamapps/workshop/content/431960')

IMG_EXT = ('.tex', '.png', '.jpg', '.jpeg', '.tga', '.bmp')


def resolve_entry(pkg, name):
    """Mirror the renderer's texture-name resolution."""
    if name.lower().endswith(IMG_EXT):
        for cand in (f"materials/{name}", name):
            if pkg.get(cand) is not None:
                return cand
        return None
    for ext in ('.tex', '.png', '.jpg', '.jpeg', '.tga', '.bmp', ''):
        cand = f"materials/{name}{ext}"
        if pkg.get(cand) is not None:
            return cand
    return None


resolved = collections.Counter()
missing = collections.Counter()
per_scene = []
visited = set()
total = 0

for d in sorted(glob.glob(root + '/*')):
    sj = os.path.join(d, 'scene.pkg')
    if not os.path.exists(sj):
        continue
    try:
        pkg = Pkg(sj)
        scene = json.loads(pkg.get('scene.json') or b'{}')
    except Exception:
        continue

    ok = bad = 0

    def walk(name, depth=0):
        global total
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
        mat = pj.get('material')
        if mat:
            mb = pkg.get(mat)
            if mb:
                try:
                    mj = json.loads(mb)
                    passes = mj.get('passes') or []
                    if passes:
                        texs = passes[0].get('textures') or []
                        if texs and isinstance(texs[0], str):
                            total += 1
                            if resolve_entry(pkg, texs[0]):
                                resolved[texs[0]] += 1
                                nonlocal_ok[0] += 1
                            else:
                                missing[texs[0]] += 1
                                nonlocal_bad[0] += 1
                except Exception:
                    pass
        for ch in (pj.get('children') or []):
            walk(ch.get('name'), depth + 1)

    nonlocal_ok = [0]
    nonlocal_bad = [0]
    for o in scene.get('objects', []):
        walk(o.get('particle'))
    per_scene.append((os.path.basename(d), nonlocal_ok[0], nonlocal_bad[0]))

# re-walk with local counters via a simpler second pass
ok_total = sum(r for r in resolved.values())
bad_total = sum(m for m in missing.values())
print(f'particle material textures referenced: {ok_total + bad_total}')
print(f'  resolve inside the package : {ok_total}')
print(f'  need the built-in library  : {bad_total}')
print('\ntop missing (built-in assets):')
for k, v in missing.most_common(15):
    print(f'  {v:4}  {k}')
print('\ntop resolved (shipped in package):')
for k, v in resolved.most_common(10):
    print(f'  {v:4}  {k}')
