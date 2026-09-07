//! Reader for the Wallpaper Engine `scene.pkg` container.
//!
//! Layout was derived from the bytes of the author's own library; see
//! `docs/FORMATS.md` for the field-by-field provenance and the verification
//! method. All integers are little-endian `u32`, strings are length-prefixed and
//! not NUL-terminated:
//!
//! ```text
//! u32    version_len
//! char[] version                  e.g. "PKGV0001"
//! u32    entry_count
//! entry_count x
//!     u32    name_len
//!     char[] name
//!     u32    offset               relative to the end of the entry table
//!     u32    size
//! <blob>
//! ```

use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::path::Path;

/// Refuse absurd lengths early rather than trying to allocate them. Real names
/// are short paths; real packages in the reference corpus hold tens of entries.
const MAX_NAME_LEN: u32 = 4096;
const MAX_ENTRIES: u32 = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub offset: u32,
    pub size: u32,
}

#[derive(Debug)]
pub struct Package {
    bytes: Vec<u8>,
    /// Where entry data begins; `Entry::offset` is relative to this.
    data_start: usize,
    pub version: String,
    pub entries: BTreeMap<String, Entry>,
}

struct Cursor<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn u32(&mut self) -> Result<u32> {
        let end = self
            .pos
            .checked_add(4)
            .context("length field past end of file")?;
        let slice = self
            .b
            .get(self.pos..end)
            .context("length field past end of file")?;
        self.pos = end;
        Ok(u32::from_le_bytes(slice.try_into().expect("4 bytes")))
    }

    fn string(&mut self, len: u32) -> Result<String> {
        if len > MAX_NAME_LEN {
            bail!("string length {len} exceeds {MAX_NAME_LEN}; not a package");
        }
        let end = self
            .pos
            .checked_add(len as usize)
            .context("string past end of file")?;
        let slice = self
            .b
            .get(self.pos..end)
            .context("string past end of file")?;
        self.pos = end;
        Ok(String::from_utf8_lossy(slice).into_owned())
    }
}

impl Package {
    pub fn open(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(bytes).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn parse(bytes: Vec<u8>) -> Result<Self> {
        let (data_start, version, entries) = {
            let mut c = Cursor { b: &bytes, pos: 0 };
            let version_len = c.u32()?;
            let version = c.string(version_len)?;
            if !version.starts_with("PKGV") {
                bail!("expected a PKGV#### version string, found {version:?}");
            }

            let count = c.u32()?;
            if count > MAX_ENTRIES {
                bail!("entry count {count} is implausible; not a package");
            }

            let mut entries = BTreeMap::new();
            for i in 0..count {
                let name_len = c.u32()?;
                let name = c
                    .string(name_len)
                    .with_context(|| format!("entry {i} name"))?;
                let offset = c.u32()?;
                let size = c.u32()?;
                entries.insert(name, Entry { offset, size });
            }
            (c.pos, version, entries)
        };

        // The entry table must end exactly where the data begins, so the last
        // byte addressed by any entry is the last byte of the file. This is the
        // check that confirmed the layout in the first place; keeping it here
        // turns a misparse into an error instead of silent garbage.
        let addressed = entries
            .values()
            .map(|e| e.offset as u64 + e.size as u64)
            .max()
            .unwrap_or(0);
        let expected = data_start as u64 + addressed;
        if expected != bytes.len() as u64 {
            bail!(
                "entry table does not account for the file: data starts at {data_start}, \
                 entries reach {addressed}, total {expected}, file is {} bytes",
                bytes.len()
            );
        }

        Ok(Package {
            bytes,
            data_start,
            version,
            entries,
        })
    }

    pub fn get(&self, name: &str) -> Option<&[u8]> {
        let e = self.entries.get(name)?;
        let start = self.data_start + e.offset as usize;
        self.bytes.get(start..start + e.size as usize)
    }

    /// Entry contents as text, tolerating the UTF-8 BOM some entries carry.
    pub fn get_str(&self, name: &str) -> Option<&str> {
        let raw = self.get(name)?;
        let raw = raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(raw);
        std::str::from_utf8(raw).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixtures are built here rather than taken from a Workshop item; shipping
    /// real wallpaper data would mean redistributing someone else's work.
    fn build(version: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut header = Vec::new();
        header.extend((version.len() as u32).to_le_bytes());
        header.extend(version.as_bytes());
        header.extend((files.len() as u32).to_le_bytes());
        let mut blob: Vec<u8> = Vec::new();
        for (name, data) in files {
            header.extend((name.len() as u32).to_le_bytes());
            header.extend(name.as_bytes());
            header.extend((blob.len() as u32).to_le_bytes());
            header.extend((data.len() as u32).to_le_bytes());
            blob.extend(*data);
        }
        header.extend(blob);
        header
    }

    #[test]
    fn reads_entries() {
        let raw = build(
            "PKGV0001",
            &[("scene.json", b"{\"a\":1}"), ("t.tex", b"\x00\x01\x02")],
        );
        let p = Package::parse(raw).unwrap();
        assert_eq!(p.version, "PKGV0001");
        assert_eq!(p.entries.len(), 2);
        assert_eq!(p.get("scene.json").unwrap(), b"{\"a\":1}");
        assert_eq!(p.get("t.tex").unwrap(), b"\x00\x01\x02");
        assert!(p.get("missing").is_none());
    }

    #[test]
    fn accepts_any_pkgv_revision() {
        // 20 distinct version strings appear in the reference corpus and all
        // share this table layout.
        let raw = build("PKGV0023", &[("scene.json", b"{}")]);
        assert_eq!(Package::parse(raw).unwrap().version, "PKGV0023");
    }

    #[test]
    fn strips_bom() {
        let raw = build("PKGV0001", &[("scene.json", b"\xef\xbb\xbf{}")]);
        assert_eq!(
            Package::parse(raw).unwrap().get_str("scene.json").unwrap(),
            "{}"
        );
    }

    #[test]
    fn rejects_trailing_slack() {
        let mut raw = build("PKGV0001", &[("a", b"xy")]);
        raw.push(0); // one byte no entry accounts for
        assert!(Package::parse(raw).is_err());
    }

    #[test]
    fn rejects_foreign_file() {
        assert!(Package::parse(b"not a package at all".to_vec()).is_err());
    }

    #[test]
    fn rejects_truncated_header() {
        let raw = build("PKGV0001", &[("scene.json", b"{}")]);
        assert!(Package::parse(raw[..8].to_vec()).is_err());
    }
}
