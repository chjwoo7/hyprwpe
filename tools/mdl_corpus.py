"""Extract every .mdl from every scene.pkg in the Workshop library.

This is the generality test bed for the MDLV parser: one parser must handle all
of them, across versions and sizes, with no per-file constants.

Usage: python3 tools/mdl_corpus.py [out_dir]   (default /tmp/mdlcorpus)
Prints a summary: per file its path, size, version tag and section tags, plus
the distinct versions/tag-sets seen, so a rule that only fits one file stands out.
"""
import glob
import os
import re
import struct
import sys

ROOT = os.path.expanduser("~/.steam/root/steamapps/workshop/content/431960")


def parse_pkg(path):
    data = open(path, "rb").read()
    pos = 0
    vlen = struct.unpack_from("<I", data, pos)[0]
    pos += 4 + vlen
    cnt = struct.unpack_from("<I", data, pos)[0]
    pos += 4
    entries = {}
    for _ in range(cnt):
        nl = struct.unpack_from("<I", data, pos)[0]
        pos += 4
        nm = data[pos:pos + nl].decode("utf-8", "replace")
        pos += nl
        off, sz = struct.unpack_from("<II", data, pos)
        pos += 8
        entries[nm] = (off, sz)
    return data, pos, entries


def main():
    out_dir = sys.argv[1] if len(sys.argv) > 1 else "/tmp/mdlcorpus"
    os.makedirs(out_dir, exist_ok=True)

    versions = {}
    tagsets = {}
    total = 0
    for d in sorted(glob.glob(os.path.join(ROOT, "*"))):
        pkg_path = os.path.join(d, "scene.pkg")
        if not os.path.exists(pkg_path):
            continue
        wid = os.path.basename(d)
        try:
            data, dstart, entries = parse_pkg(pkg_path)
        except Exception as e:
            print(f"{wid}: pkg parse failed: {e}")
            continue
        for name, (off, sz) in sorted(entries.items()):
            if not name.lower().endswith(".mdl"):
                continue
            total += 1
            blob = data[dstart + off:dstart + off + sz]
            tags = [m.group().decode() for m in re.finditer(rb"[A-Z]{4}\d{4}", blob)]
            version = tags[0] if tags else "?"
            versions[version] = versions.get(version, 0) + 1
            tagset = ",".join(sorted(set(tags[1:])))
            tagsets[tagset] = tagsets.get(tagset, 0) + 1

            safe = f"{wid}__" + name.replace("/", "_")
            with open(os.path.join(out_dir, safe), "wb") as fh:
                fh.write(blob)
            print(f"{wid:<12} {sz:>9}  {version:<9} {', '.join(tags[1:])}")

    print(f"\nmdl files: {total}")
    print(f"versions: {versions}")
    print("tag sets:")
    for k, v in sorted(tagsets.items(), key=lambda kv: -kv[1]):
        print(f"  {v:>3}x  {k}")
    print(f"\nextracted to {out_dir}")


if __name__ == "__main__":
    main()
