"""Extract .mdl (and any other entry) from Wallpaper Engine scene.pkg files.

Usage: python3 tools/extract_pkg.py <scene.pkg> <entry> <out>
       python3 tools/extract_pkg.py <scene.pkg> --list
"""
import struct, sys

def parse_pkg(path):
    data = open(path, 'rb').read()
    pos = 0
    vlen = struct.unpack_from('<I', data, pos)[0]; pos += 4
    pos += vlen
    cnt = struct.unpack_from('<I', data, pos)[0]; pos += 4
    entries = {}
    for _ in range(cnt):
        nl = struct.unpack_from('<I', data, pos)[0]; pos += 4
        nm = data[pos:pos+nl].decode('utf-8', 'replace'); pos += nl
        off, sz = struct.unpack_from('<II', data, pos); pos += 8
        entries[nm] = (off, sz)
    return data, pos, entries

def main():
    pkg_path, arg, *rest = sys.argv[1:]
    data, dstart, entries = parse_pkg(pkg_path)
    if arg == '--list':
        for n in sorted(entries):
            print(f"{entries[n][1]:>10}  {n}")
        return
    out = rest[0]
    off, sz = entries[arg]
    open(out, 'wb').write(data[dstart+off:dstart+off+sz])
    print(f"wrote {out} ({sz} bytes)")

if __name__ == '__main__':
    main()
