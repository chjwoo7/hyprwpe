"""Read Wallpaper Engine `scene.pkg` containers.

The entry table's offsets are relative to the position *after* the table, not
to the entry's own row - a mistake that silently yields garbage.

Usage:
    from pkgread import Pkg
    pkg = Pkg("/path/scene.pkg")
    pkg.names()            # sorted entry names
    pkg.get("scene.json")  # bytes or None
"""
import struct


class Pkg:
    def __init__(self, path):
        self.data = open(path, 'rb').read()
        data = self.data
        pos = 0
        vlen = struct.unpack_from('<I', data, pos)[0]
        pos += 4 + vlen  # version string, e.g. "PKGV0016"
        count = struct.unpack_from('<I', data, pos)[0]
        pos += 4
        rows = []
        for _ in range(count):
            nl = struct.unpack_from('<I', data, pos)[0]
            pos += 4
            name = data[pos:pos + nl].decode('utf-8', 'replace')
            pos += nl
            off, size = struct.unpack_from('<II', data, pos)
            pos += 8
            rows.append((name, off, size))
        # Offsets are relative to the end of the table.
        self.start = pos
        self.entries = {n: (off, size) for n, off, size in rows}

    def names(self):
        return sorted(self.entries)

    def get(self, name):
        if name not in self.entries:
            return None
        off, size = self.entries[name]
        return self.data[self.start + off:self.start + off + size]

    def get_str(self, name):
        body = self.get(name)
        return None if body is None else body.decode('utf-8', 'replace')
