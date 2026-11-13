//! A minimal read-only zip reader over a memory map.
//!
//! Each entry is inflated straight from the map, so any number of threads can read at once with no lock.
//! Handles stored and deflated entries and zip64 records (LDraw's complete.zip has more than 65,535 entries).

use std::{fs::File, io, path::Path};

use memmap2::Mmap;

use crate::num::fnv1a64;

/// One file in the archive.
#[derive(Clone, Debug)]
pub struct ZipEntry {
    /// Name as stored (case preserved, `/` separators).
    pub name: String,
    offset: u64,
    csize: u64,
    usize: u64,
    method: u16,
}

impl ZipEntry {
    /// Uncompressed size in bytes.
    pub fn size(&self) -> u64 {
        self.usize
    }
}

pub struct ZipFs {
    map: Mmap,
    entries: Vec<ZipEntry>,
    fingerprint: u64,
}

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

struct Rd<'a>(&'a [u8]);

impl<'a> Rd<'a> {
    fn bytes(&self, o: usize, n: usize) -> io::Result<&'a [u8]> {
        let end = o.checked_add(n).ok_or_else(|| bad("zip offset overflow"))?;
        self.0.get(o..end).ok_or_else(|| bad("truncated zip"))
    }
    fn u16(&self, o: usize) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.bytes(o, 2)?.try_into().unwrap()))
    }
    fn u32(&self, o: usize) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.bytes(o, 4)?.try_into().unwrap()))
    }
    fn u64(&self, o: usize) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.bytes(o, 8)?.try_into().unwrap()))
    }
}

const EOCD: u32 = 0x0605_4b50;
const EOCD64_LOCATOR: u32 = 0x0706_4b50;
const EOCD64: u32 = 0x0606_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const LOCAL: u32 = 0x0403_4b50;

impl ZipFs {
    pub fn open(path: &Path) -> io::Result<ZipFs> {
        let file = File::open(path)?;
        // SAFETY: the library archive is opened read-only and is not expected to change while mapped.
        let map = unsafe { Mmap::map(&file)? };
        let r = Rd(&map[..]);
        let len = map.len();
        if len < 22 {
            return Err(bad("not a zip file (too small)"));
        }
        let lowest = len.saturating_sub(22 + 0xFFFF);
        let mut e = len - 22;
        let eocd = loop {
            if r.u32(e)? == EOCD {
                break e;
            }
            if e == lowest {
                return Err(bad("zip end-of-central-directory record not found"));
            }
            e -= 1;
        };
        let mut count = r.u16(eocd + 10)? as u64;
        let mut cd_size = r.u32(eocd + 12)? as u64;
        let mut cd_off = r.u32(eocd + 16)? as u64;
        if (count == 0xFFFF || cd_size == 0xFFFF_FFFF || cd_off == 0xFFFF_FFFF)
            && eocd >= 20
            && r.u32(eocd - 20)? == EOCD64_LOCATOR
        {
            let z = r.u64(eocd - 20 + 8)? as usize;
            if r.u32(z)? == EOCD64 {
                count = r.u64(z + 32)?;
                cd_size = r.u64(z + 40)?;
                cd_off = r.u64(z + 48)?;
            }
        }
        let cd = r.bytes(cd_off as usize, cd_size as usize)?;
        let fingerprint = fnv1a64(cd) ^ (len as u64).rotate_left(32);

        let mut entries = Vec::with_capacity(count as usize);
        let mut p = cd_off as usize;
        for _ in 0..count {
            if r.u32(p)? != CENTRAL {
                return Err(bad("bad zip central directory entry"));
            }
            let flags = r.u16(p + 8)?;
            let method = r.u16(p + 10)?;
            let mut csize = r.u32(p + 20)? as u64;
            let mut usize_ = r.u32(p + 24)? as u64;
            let nlen = r.u16(p + 28)? as usize;
            let elen = r.u16(p + 30)? as usize;
            let clen = r.u16(p + 32)? as usize;
            let mut offset = r.u32(p + 42)? as u64;
            let name_b = r.bytes(p + 46, nlen)?;
            let extra = r.bytes(p + 46 + nlen, elen)?;
            let mut q = 0;
            while q + 4 <= extra.len() {
                let id = u16::from_le_bytes([extra[q], extra[q + 1]]);
                let sz = u16::from_le_bytes([extra[q + 2], extra[q + 3]]) as usize;
                if id == 1 {
                    let body = extra.get(q + 4..q + 4 + sz).unwrap_or(&[]);
                    let mut k = 0;
                    let mut next = || {
                        let v = body.get(k..k + 8).map(|s| u64::from_le_bytes(s.try_into().unwrap()));
                        k += 8;
                        v
                    };
                    if usize_ == 0xFFFF_FFFF {
                        usize_ = next().unwrap_or(usize_);
                    }
                    if csize == 0xFFFF_FFFF {
                        csize = next().unwrap_or(csize);
                    }
                    if offset == 0xFFFF_FFFF {
                        offset = next().unwrap_or(offset);
                    }
                }
                q += 4 + sz;
            }
            let name = if flags & 0x800 != 0 {
                String::from_utf8_lossy(name_b).into_owned()
            } else {
                String::from_utf8(name_b.to_vec()).unwrap_or_else(|_| name_b.iter().map(|&b| b as char).collect())
            };
            if !name.ends_with('/') {
                entries.push(ZipEntry { name, offset, csize, usize: usize_, method });
            }
            p += 46 + nlen + elen + clen;
        }
        Ok(ZipFs { map, entries, fingerprint })
    }

    pub fn entries(&self) -> &[ZipEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// A stable stamp of the archive contents (central directory hash and file size).
    pub fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    fn data(&self, e: &ZipEntry) -> io::Result<&[u8]> {
        let r = Rd(&self.map[..]);
        let lh = e.offset as usize;
        if r.u32(lh)? != LOCAL {
            return Err(bad(format!("bad zip local header for {}", e.name)));
        }
        let start = lh + 30 + r.u16(lh + 26)? as usize + r.u16(lh + 28)? as usize;
        r.bytes(start, e.csize as usize)
    }

    /// The whole uncompressed content of entry `i`.
    pub fn read(&self, i: usize) -> io::Result<Vec<u8>> {
        let e = &self.entries[i];
        let data = self.data(e)?;
        match e.method {
            0 => Ok(data.to_vec()),
            8 => miniz_oxide::inflate::decompress_to_vec_with_limit(data, (e.usize as usize).max(16))
                .map_err(|err| bad(format!("inflate {}: {:?}", e.name, err.status))),
            m => Err(bad(format!("unsupported zip compression method {m} for {}", e.name))),
        }
    }

    /// At most about `max` bytes from the start of entry `i` (cheap first-line reads).
    pub fn read_prefix(&self, i: usize, max: usize) -> io::Result<Vec<u8>> {
        let e = &self.entries[i];
        let data = self.data(e)?;
        match e.method {
            0 => Ok(data[..data.len().min(max)].to_vec()),
            8 => match miniz_oxide::inflate::decompress_to_vec_with_limit(data, max) {
                Ok(v) => Ok(v),
                Err(err) if err.status == miniz_oxide::inflate::TINFLStatus::HasMoreOutput => Ok(err.output),
                Err(err) => Err(bad(format!("inflate {}: {:?}", e.name, err.status))),
            },
            m => Err(bad(format!("unsupported zip compression method {m} for {}", e.name))),
        }
    }
}
