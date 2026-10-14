//! NTFS: finds which files own given clusters, by reading the `$MFT` (read-only) and decoding its data runs.
//!
//! Everything here is pure parsing over a [`VolumeRead`]; the Windows volume handle is in [`WinVolume`]. The approach:
//! read the boot sector, decode the runs of `$MFT` (record 0), stream through every file record and keep the non-resident
//! attributes whose runs touch the requested clusters, then resolve names of just those files by reading their records
//! (and their parents) directly. `$Bitmap` (record 6) tells free clusters from allocated ones.
//!
//! Honest limits: only plain (single-extent) volumes are mapped; names come from the file's `$FILE_NAME` (Win32 name
//! preferred over the DOS one); hard links show one name; resident files (tiny files stored inside the MFT record) own no
//! clusters; compressed / encrypted files are reported by their stored clusters.

use std::collections::HashMap;

use crate::error::{Error, Result};

pub trait VolumeRead {
    /// Reads exactly `buf.len()` bytes at `offset` (an implementation may need to align internally).
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<()>;
}

fn bad(msg: &str) -> Error {
    Error::Parse(format!("NTFS: {msg}"))
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().expect("4 bytes"))
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().expect("8 bytes"))
}

const REF_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
pub const ROOT_RECORD: u64 = 5;
const MFT_RECORD: u64 = 0;
const BITMAP_RECORD: u64 = 6;
const FIRST_USER_RECORD: u64 = 16;

pub const ATTR_ATTRIBUTE_LIST: u32 = 0x20;
pub const ATTR_FILE_NAME: u32 = 0x30;
pub const ATTR_DATA: u32 = 0x80;
pub const ATTR_INDEX_ALLOCATION: u32 = 0xA0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Boot {
    pub bytes_per_sector: u32,
    pub cluster_size: u64,
    pub mft_lcn: u64,
    pub record_size: u32,
    pub total_clusters: u64,
}

pub fn parse_boot(s: &[u8]) -> Result<Boot> {
    if s.len() < 0x50 || &s[3..11] != b"NTFS    " {
        return Err(bad("not an NTFS boot sector"));
    }
    let bytes_per_sector = u32::from(u16_at(s, 0x0B));
    let spc = s[0x0D];
    let sectors_per_cluster = if spc > 0x80 {
        1u64 << (256 - u32::from(spc))
    } else {
        u64::from(spc)
    };
    let cluster_size = u64::from(bytes_per_sector) * sectors_per_cluster;
    if !(512..=65536).contains(&bytes_per_sector) || cluster_size == 0 || !cluster_size.is_power_of_two() {
        return Err(bad("implausible sector or cluster size"));
    }
    let cpr = s[0x40] as i8;
    let record_size = if cpr > 0 {
        cpr as u64 * cluster_size
    } else {
        1u64 << (-i32::from(cpr))
    };
    if !(512..=65536).contains(&record_size) {
        return Err(bad("implausible MFT record size"));
    }
    Ok(Boot {
        bytes_per_sector,
        cluster_size,
        mft_lcn: u64_at(s, 0x30),
        record_size: record_size as u32,
        total_clusters: u64_at(s, 0x28) * u64::from(bytes_per_sector) / cluster_size,
    })
}

/// Undoes the update-sequence protection of a multi-sector record in place.
pub fn apply_fixups(rec: &mut [u8]) -> Result<()> {
    if rec.len() < 0x30 || &rec[0..4] != b"FILE" {
        return Err(bad("record has no FILE signature"));
    }
    let (usa, count) = (usize::from(u16_at(rec, 4)), usize::from(u16_at(rec, 6)));
    if count < 2 || usa + count * 2 > rec.len() || (count - 1) * 512 > rec.len() {
        return Err(bad("bad update sequence array"));
    }
    let usn = [rec[usa], rec[usa + 1]];
    for i in 1..count {
        let end = i * 512;
        if rec[end - 2..end] != usn {
            return Err(bad("update sequence mismatch (torn record)"));
        }
        rec[end - 2] = rec[usa + i * 2];
        rec[end - 1] = rec[usa + i * 2 + 1];
    }
    Ok(())
}

/// A piece of a stream: `len` clusters from virtual cluster `vcn`; `lcn` is `None` for a sparse hole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    pub vcn: u64,
    pub lcn: Option<u64>,
    pub len: u64,
}

/// Decodes NTFS mapping pairs starting at virtual cluster `start_vcn`.
pub fn decode_runs(b: &[u8], start_vcn: u64) -> Result<Vec<Run>> {
    let (mut i, mut vcn, mut lcn) = (0usize, start_vcn, 0i64);
    let mut out = Vec::new();
    while i < b.len() && b[i] != 0 {
        let (ls, os) = (usize::from(b[i] & 0x0F), usize::from(b[i] >> 4));
        i += 1;
        if ls == 0 || ls > 8 || os > 8 || i + ls + os > b.len() {
            return Err(bad("corrupt data run"));
        }
        let mut len = 0u64;
        for k in 0..ls {
            len |= u64::from(b[i + k]) << (8 * k);
        }
        i += ls;
        let l = if os == 0 {
            None
        } else {
            let mut d = 0i64;
            for k in 0..os {
                d |= i64::from(b[i + k]) << (8 * k);
            }
            if b[i + os - 1] & 0x80 != 0 && os < 8 {
                d |= -1i64 << (8 * os); // sign extend
            }
            i += os;
            lcn = lcn
                .checked_add(d)
                .filter(|v| *v >= 0)
                .ok_or_else(|| bad("data run points before the volume start"))?;
            Some(lcn as u64)
        };
        out.push(Run { vcn, lcn: l, len });
        vcn += len;
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileName {
    pub parent: u64,
    pub name: String,
    pub namespace: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stream {
    pub attr_type: u32,
    pub name: String,
    pub real_size: u64,
    pub runs: Vec<Run>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub number: u64,
    pub is_dir: bool,
    /// Record number of the base record when this is an extension record.
    pub base: Option<u64>,
    pub names: Vec<FileName>,
    /// Non-resident attributes.
    pub streams: Vec<Stream>,
}

impl Record {
    /// The record this one's data belongs to.
    pub fn owner(&self) -> u64 {
        self.base.unwrap_or(self.number)
    }

    pub fn best_name(&self) -> Option<&FileName> {
        // Win32 (1) or Win32+DOS (3), then POSIX (0), DOS (2) last
        let rank = |n: &FileName| match n.namespace {
            1 | 3 => 0,
            0 => 1,
            _ => 2,
        };
        self.names.iter().min_by_key(|n| rank(n))
    }
}

/// Parses one (already fixed-up) record. `None` for unused records and anything that is not a FILE record.
pub fn parse_record(number: u64, rec: &[u8]) -> Result<Option<Record>> {
    if rec.len() < 0x30 || &rec[0..4] != b"FILE" {
        return Ok(None);
    }
    let flags = u16_at(rec, 0x16);
    if flags & 1 == 0 {
        return Ok(None);
    }
    let base = u64_at(rec, 0x20) & REF_MASK;
    let mut r = Record {
        number,
        is_dir: flags & 2 != 0,
        base: (base != 0).then_some(base),
        names: Vec::new(),
        streams: Vec::new(),
    };
    let used = (u32_at(rec, 0x18) as usize).min(rec.len());
    let mut o = usize::from(u16_at(rec, 0x14));
    while o + 16 <= used {
        let ty = u32_at(rec, o);
        if ty == 0xFFFF_FFFF {
            break;
        }
        let len = u32_at(rec, o + 4) as usize;
        if len < 16 || o + len > used {
            return Err(bad("attribute runs past the record"));
        }
        let attr = &rec[o..o + len];
        let name_len = usize::from(attr[9]);
        let name_off = usize::from(u16_at(attr, 0x0A));
        let stream_name = if name_len > 0 && name_off + name_len * 2 <= len {
            String::from_utf16_lossy(
                &attr[name_off..name_off + name_len * 2]
                    .chunks(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect::<Vec<u16>>(),
            )
        } else {
            String::new()
        };
        if attr[8] == 0 {
            if ty == ATTR_FILE_NAME {
                let (vo, vl) = (usize::from(u16_at(attr, 0x14)), u32_at(attr, 0x10) as usize);
                if vo + vl <= len && vl >= 0x42 {
                    let v = &attr[vo..vo + vl];
                    let n = usize::from(v[0x40]);
                    if 0x42 + n * 2 <= vl {
                        let units: Vec<u16> = v[0x42..0x42 + n * 2]
                            .chunks(2)
                            .map(|c| u16::from_le_bytes([c[0], c[1]]))
                            .collect();
                        r.names.push(FileName {
                            parent: u64_at(v, 0) & REF_MASK,
                            name: String::from_utf16_lossy(&units),
                            namespace: v[0x41],
                        });
                    }
                }
            }
        } else if len >= 0x40 {
            let (ro, start_vcn) = (usize::from(u16_at(attr, 0x20)), u64_at(attr, 0x10));
            if ro < len {
                r.streams.push(Stream {
                    attr_type: ty,
                    name: stream_name,
                    real_size: u64_at(attr, 0x30),
                    runs: decode_runs(&attr[ro..], start_vcn)?,
                });
            }
        }
        o += len;
    }
    Ok(Some(r))
}

/// Reads `[off, off + len)` of a non-resident stream described by `runs` (holes read as zeros).
pub fn read_stream(vol: &dyn VolumeRead, cluster: u64, runs: &[Run], off: u64, len: usize) -> Result<Vec<u8>> {
    let mut out = vec![0u8; len];
    let end = off + len as u64;
    for run in runs {
        let (rs, re) = (run.vcn * cluster, (run.vcn + run.len) * cluster);
        let (from, to) = (off.max(rs), end.min(re));
        if from >= to {
            continue;
        }
        if let Some(lcn) = run.lcn {
            let dst = &mut out[(from - off) as usize..(to - off) as usize];
            vol.read_exact_at(lcn * cluster + (from - rs), dst)?;
        }
    }
    Ok(out)
}

/// A sorted, disjoint list of cluster ranges `[start, end)`.
#[derive(Debug, Clone, Default)]
pub struct Targets(Vec<(u64, u64)>);

impl Targets {
    pub fn new(mut v: Vec<(u64, u64)>) -> Targets {
        v.retain(|(a, b)| a < b);
        v.sort_unstable();
        let mut out: Vec<(u64, u64)> = Vec::new();
        for (a, b) in v {
            match out.last_mut() {
                Some(l) if a <= l.1 => l.1 = l.1.max(b),
                _ => out.push((a, b)),
            }
        }
        Targets(out)
    }

    /// The part of `[a, b)` inside targets, as ranges.
    pub fn intersect(&self, a: u64, b: u64) -> Vec<(u64, u64)> {
        let i = self.0.partition_point(|(_, e)| *e <= a);
        self.0[i..]
            .iter()
            .take_while(|(s, _)| *s < b)
            .map(|(s, e)| ((*s).max(a), (*e).min(b)))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// The file (base record) owning the clusters.
    pub record: u64,
    pub attr_type: u32,
    pub stream: String,
    pub lcn: u64,
    pub clusters: u64,
}

/// Where `$MFT` lives on the volume.
pub struct Mft {
    pub runs: Vec<Run>,
    pub real_size: u64,
}

fn read_record_at(vol: &dyn VolumeRead, boot: &Boot, runs: &[Run], number: u64) -> Result<Vec<u8>> {
    let mut rec = read_stream(
        vol,
        boot.cluster_size,
        runs,
        number * u64::from(boot.record_size),
        boot.record_size as usize,
    )?;
    apply_fixups(&mut rec)?;
    Ok(rec)
}

pub fn open_mft(vol: &dyn VolumeRead, boot: &Boot) -> Result<Mft> {
    let mut rec = vec![0u8; boot.record_size as usize];
    vol.read_exact_at(boot.mft_lcn * boot.cluster_size, &mut rec)?;
    apply_fixups(&mut rec)?;
    let r = parse_record(MFT_RECORD, &rec)?.ok_or_else(|| bad("$MFT record 0 is not in use"))?;
    let s = r
        .streams
        .into_iter()
        .find(|s| s.attr_type == ATTR_DATA && s.name.is_empty())
        .ok_or_else(|| bad("$MFT has no data stream"))?;
    Ok(Mft {
        runs: s.runs,
        real_size: s.real_size,
    })
}

/// Streams through every record of the MFT and returns the clusters of non-resident attributes that touch `targets`.
/// Records of deleted files are ignored; extension records are credited to their base record.
pub fn find_hits(
    vol: &dyn VolumeRead,
    boot: &Boot,
    mft: &Mft,
    targets: &Targets,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Vec<Hit>> {
    let rs = u64::from(boot.record_size);
    let mut hits = Vec::new();
    let total = mft.real_size / rs;
    let chunk_records = (4 << 20) / rs;
    let mut n = 0u64;
    while n < total {
        let count = chunk_records.min(total - n);
        let mut chunk = read_stream(vol, boot.cluster_size, &mft.runs, n * rs, (count * rs) as usize)?;
        for i in 0..count {
            let rec = &mut chunk[(i * rs) as usize..((i + 1) * rs) as usize];
            if apply_fixups(rec).is_err() {
                continue; // zeroed / torn record: nothing to say about it
            }
            let Some(r) = parse_record(n + i, rec)? else { continue };
            for s in &r.streams {
                for run in &s.runs {
                    let Some(lcn) = run.lcn else { continue };
                    for (a, b) in targets.intersect(lcn, lcn + run.len) {
                        hits.push(Hit {
                            record: r.owner(),
                            attr_type: s.attr_type,
                            stream: s.name.clone(),
                            lcn: a,
                            clusters: b - a,
                        });
                    }
                }
            }
        }
        n += count;
        progress(n, total);
    }
    Ok(hits)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
    /// NTFS system files (`$MFT`, `$LogFile`, `$Bitmap`, ...).
    Metadata,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHit {
    pub path: String,
    pub kind: Kind,
    pub record: u64,
    /// `data`, `directory index`, `attribute list`, `other`, with a stream name for alternate data streams.
    pub attr: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ZoneMap {
    pub total_bytes: u64,
    pub free_bytes: u64,
    /// Allocated clusters that no file record claims (e.g. clusters of files whose extension records were not seen).
    pub other_allocated_bytes: u64,
    pub files: Vec<FileHit>,
}

fn attr_label(ty: u32, stream: &str) -> String {
    let base = match ty {
        ATTR_DATA => "data",
        ATTR_INDEX_ALLOCATION => "directory index",
        ATTR_ATTRIBUTE_LIST => "attribute list",
        _ => "other",
    };
    if stream.is_empty() || ty == ATTR_INDEX_ALLOCATION {
        base.to_owned()
    } else {
        format!("{base}:{stream}")
    }
}

struct Resolver<'a> {
    vol: &'a dyn VolumeRead,
    boot: &'a Boot,
    mft: &'a Mft,
    cache: HashMap<u64, Option<(String, u64, bool)>>,
}

impl Resolver<'_> {
    fn node(&mut self, n: u64) -> Option<(String, u64, bool)> {
        if let Some(v) = self.cache.get(&n) {
            return v.clone();
        }
        let v = read_record_at(self.vol, self.boot, &self.mft.runs, n)
            .ok()
            .and_then(|rec| parse_record(n, &rec).ok().flatten())
            .and_then(|r| r.best_name().map(|f| (f.name.clone(), f.parent, r.is_dir)));
        self.cache.insert(n, v.clone());
        v
    }

    /// `\dir\sub\file.ext`; a broken parent chain yields `\<unknown>\...`.
    fn path(&mut self, n: u64) -> Option<(String, bool)> {
        let (name, mut parent, is_dir) = self.node(n)?;
        let mut parts = vec![name];
        for _ in 0..64 {
            if parent == ROOT_RECORD {
                parts.reverse();
                return Some((format!("\\{}", parts.join("\\")), is_dir));
            }
            match self.node(parent) {
                Some((pn, pp, _)) => {
                    parts.push(pn);
                    parent = pp;
                }
                None => {
                    parts.push("<unknown>".into());
                    parts.reverse();
                    return Some((format!("\\{}", parts.join("\\")), is_dir));
                }
            }
        }
        parts.push("...".into());
        parts.reverse();
        Some((format!("\\{}", parts.join("\\")), is_dir))
    }
}

/// Allocated cluster count in `[a, b)` according to `$Bitmap`.
fn allocated_in(bitmap: &[u8], base_lcn: u64, a: u64, b: u64) -> u64 {
    (a..b)
        .filter(|c| {
            bitmap
                .get(((c - base_lcn) / 8) as usize)
                .is_some_and(|byte| byte >> ((c - base_lcn) % 8) & 1 == 1)
        })
        .count() as u64
}

/// Maps volume byte ranges (`[start, end)` from the volume start) to the files that own them.
pub fn map_ranges(
    vol: &dyn VolumeRead,
    ranges: &[(u64, u64)],
    progress: &mut dyn FnMut(u64, u64),
) -> Result<(Boot, Vec<ZoneMap>)> {
    let mut sector = vec![0u8; 512];
    vol.read_exact_at(0, &mut sector)?;
    let boot = parse_boot(&sector)?;
    let c = boot.cluster_size;
    let clusters: Vec<(u64, u64)> = ranges
        .iter()
        .map(|(a, b)| (a / c, b.div_ceil(c).min(boot.total_clusters)))
        .collect();
    let targets = Targets::new(clusters.clone());
    let mft = open_mft(vol, &boot)?;
    let hits = find_hits(vol, &boot, &mft, &targets, progress)?;
    // $Bitmap: only the bytes covering the targets
    let bm = parse_record(BITMAP_RECORD, &read_record_at(vol, &boot, &mft.runs, BITMAP_RECORD)?)?
        .and_then(|r| r.streams.into_iter().find(|s| s.attr_type == ATTR_DATA));
    let mut res = Resolver {
        vol,
        boot: &boot,
        mft: &mft,
        cache: HashMap::new(),
    };
    let mut out = Vec::new();
    for &(a, b) in &clusters {
        let mut z = ZoneMap {
            total_bytes: (b.saturating_sub(a)) * c,
            ..ZoneMap::default()
        };
        let mut by_key: HashMap<(u64, u32, String), u64> = HashMap::new();
        for h in &hits {
            let (lo, hi) = (h.lcn.max(a), (h.lcn + h.clusters).min(b));
            if lo < hi {
                *by_key.entry((h.record, h.attr_type, h.stream.clone())).or_default() += (hi - lo) * c;
            }
        }
        let claimed: u64 = by_key.values().sum();
        for ((rec, ty, stream), bytes) in by_key {
            let (path, is_dir) = res.path(rec).unwrap_or_else(|| (format!("<record {rec}>"), false));
            let kind = if rec < FIRST_USER_RECORD {
                Kind::Metadata
            } else if is_dir {
                Kind::Directory
            } else {
                Kind::File
            };
            z.files.push(FileHit {
                path,
                kind,
                record: rec,
                attr: attr_label(ty, &stream),
                bytes,
            });
        }
        z.files
            .sort_by(|x, y| y.bytes.cmp(&x.bytes).then_with(|| x.path.cmp(&y.path)));
        if let Some(bm) = &bm {
            let (b0, b1) = (a / 8, (b.div_ceil(8)).min(bm.real_size));
            if b1 > b0 {
                let bytes = read_stream(vol, c, &bm.runs, b0, (b1 - b0) as usize)?;
                let alloc = allocated_in(&bytes, b0 * 8, a, b);
                z.free_bytes = (b - a - alloc) * c;
                z.other_allocated_bytes = (alloc * c).saturating_sub(claimed);
            }
        }
        out.push(z);
    }
    Ok((boot, out))
}

// ---- Windows volume ---------------------------------------------------------------------------------------------

/// A volume (`\\.\C:`) opened read-only.
#[cfg(windows)]
pub struct WinVolume(crate::winapi::Device);

#[cfg(windows)]
impl WinVolume {
    pub fn open(letter: &str) -> Result<WinVolume> {
        Ok(WinVolume(crate::winapi::Device::open_read_only_unbuffered(&format!(
            r"\\.\{}",
            letter.trim_end_matches('\\')
        ))?))
    }
}

#[cfg(windows)]
impl VolumeRead for WinVolume {
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        // unbuffered reads must be sector aligned in offset, length and memory
        const A: u64 = 4096;
        let start = offset / A * A;
        let end = (offset + buf.len() as u64).div_ceil(A) * A;
        let mut tmp = crate::surface::AlignedBuf::new((end - start) as usize);
        let mut got = 0usize;
        while got < tmp.as_slice().len() {
            let n = self.0.read_at(start + got as u64, &mut tmp.as_mut_slice()[got..])?;
            if n == 0 {
                return Err(Error::Parse("NTFS: short read".into()));
            }
            got += n;
        }
        buf.copy_from_slice(&tmp.as_slice()[(offset - start) as usize..(offset - start) as usize + buf.len()]);
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const CLUSTER: u64 = 4096;
    pub(crate) const REC: usize = 1024;

    pub(crate) struct Image(pub Vec<u8>);

    impl VolumeRead for Image {
        fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
            let (o, e) = (offset as usize, offset as usize + buf.len());
            if e > self.0.len() {
                return Err(Error::Os {
                    op: "ReadFile",
                    code: 23,
                });
            }
            buf.copy_from_slice(&self.0[o..e]);
            Ok(())
        }
    }

    fn encode_runs(runs: &[(Option<u64>, u64)]) -> Vec<u8> {
        let (mut out, mut prev) = (Vec::new(), 0i64);
        for &(lcn, len) in runs {
            let lb = len.to_le_bytes();
            let ln = lb.iter().rposition(|b| *b != 0).map_or(1, |p| p + 1);
            match lcn {
                None => {
                    out.push(ln as u8);
                    out.extend_from_slice(&lb[..ln]);
                }
                Some(l) => {
                    let d = l as i64 - prev;
                    prev = l as i64;
                    let db = d.to_le_bytes();
                    // minimal signed encoding
                    let mut on = 8;
                    while on > 1
                        && ((db[on - 1] == 0 && db[on - 2] & 0x80 == 0)
                            || (db[on - 1] == 0xFF && db[on - 2] & 0x80 != 0))
                    {
                        on -= 1;
                    }
                    out.push((on as u8) << 4 | ln as u8);
                    out.extend_from_slice(&lb[..ln]);
                    out.extend_from_slice(&db[..on]);
                }
            }
        }
        out.push(0);
        out
    }

    pub(crate) struct Rec {
        b: Vec<u8>,
        pos: usize,
    }

    impl Rec {
        pub(crate) fn new(in_use: bool, dir: bool, base: u64) -> Rec {
            let mut b = vec![0u8; REC];
            b[0..4].copy_from_slice(b"FILE");
            b[4..6].copy_from_slice(&0x30u16.to_le_bytes()); // USA offset
            b[6..8].copy_from_slice(&3u16.to_le_bytes()); // USN + 2 sectors
            b[0x14..0x16].copy_from_slice(&0x38u16.to_le_bytes());
            b[0x16..0x18].copy_from_slice(&(u16::from(in_use) | u16::from(dir) << 1).to_le_bytes());
            b[0x1C..0x20].copy_from_slice(&(REC as u32).to_le_bytes());
            b[0x20..0x28].copy_from_slice(&base.to_le_bytes());
            Rec { b, pos: 0x38 }
        }

        fn attr(&mut self, ty: u32, nonres: bool, name: &str, body: &[u8]) {
            let name_units: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
            let head = if nonres { 0x40 } else { 0x18 };
            let name_off = head;
            let body_off = (head + name_units.len()).div_ceil(8) * 8;
            let len = (body_off + body.len()).div_ceil(8) * 8;
            let a = &mut self.b[self.pos..self.pos + len];
            a[0..4].copy_from_slice(&ty.to_le_bytes());
            a[4..8].copy_from_slice(&(len as u32).to_le_bytes());
            a[8] = u8::from(nonres);
            a[9] = (name_units.len() / 2) as u8;
            a[0x0A..0x0C].copy_from_slice(&(name_off as u16).to_le_bytes());
            a[name_off..name_off + name_units.len()].copy_from_slice(&name_units);
            a[body_off..body_off + body.len()].copy_from_slice(body);
            if nonres {
                a[0x20..0x22].copy_from_slice(&(body_off as u16).to_le_bytes());
            } else {
                a[0x10..0x14].copy_from_slice(&(body.len() as u32).to_le_bytes());
                a[0x14..0x16].copy_from_slice(&(body_off as u16).to_le_bytes());
            }
            self.pos += len;
        }

        pub(crate) fn file_name(mut self, parent: u64, name: &str, ns: u8) -> Rec {
            let units: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
            let mut v = vec![0u8; 0x42 + units.len()];
            v[0..8].copy_from_slice(&(parent | 1 << 48).to_le_bytes());
            v[0x40] = (units.len() / 2) as u8;
            v[0x41] = ns;
            v[0x42..].copy_from_slice(&units);
            self.attr(ATTR_FILE_NAME, false, "", &v);
            self
        }

        pub(crate) fn nonres(mut self, ty: u32, name: &str, runs: &[(Option<u64>, u64)], real_size: u64) -> Rec {
            // the stream starts at VCN 0; the run list is stored after a 0x40-byte header
            let mut body = encode_runs(runs);
            body.resize(body.len().max(8), 0);
            let total: u64 = runs.iter().map(|r| r.1).sum();
            let start = self.pos;
            self.attr(ty, true, name, &body);
            let a = &mut self.b[start..];
            a[0x18..0x20].copy_from_slice(&total.saturating_sub(1).to_le_bytes());
            a[0x28..0x30].copy_from_slice(&(total * CLUSTER).to_le_bytes());
            a[0x30..0x38].copy_from_slice(&real_size.to_le_bytes());
            self
        }

        /// Ends the attribute list and applies the update sequence protection.
        pub(crate) fn finish(mut self) -> Vec<u8> {
            self.b[self.pos..self.pos + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
            let used = (self.pos + 8) as u32;
            self.b[0x18..0x1C].copy_from_slice(&used.to_le_bytes());
            let usn = [0x07u8, 0x00];
            self.b[0x30..0x32].copy_from_slice(&usn);
            for i in 1..3 {
                let end = i * 512;
                let saved = [self.b[end - 2], self.b[end - 1]];
                self.b[0x30 + i * 2..0x30 + i * 2 + 2].copy_from_slice(&saved);
                self.b[end - 2..end].copy_from_slice(&usn);
            }
            self.b
        }
    }

    /// A 128-cluster volume image. Layout (clusters): 0 boot, 4..12 MFT (32 records), 12 $Bitmap, 20 dir index of
    /// `\Users`, 30..34 + 40..42 `\Users\big.bin`, 50..52 `\Users\win32.bin` (also has a DOS name), 55..57 its extension
    /// record, 60..62 `\Users\holes.bin` (with a hole), 70..72 a deleted file, 80..84 `$LogFile`.
    pub(crate) fn image() -> Image {
        let mut v = vec![0u8; 128 * CLUSTER as usize];
        v[3..11].copy_from_slice(b"NTFS    ");
        v[0x0B..0x0D].copy_from_slice(&512u16.to_le_bytes());
        v[0x0D] = 8;
        v[0x28..0x30].copy_from_slice(&(128u64 * 8).to_le_bytes());
        v[0x30..0x38].copy_from_slice(&4u64.to_le_bytes());
        v[0x40] = (-10i8) as u8; // 1024-byte records
        let mut put = |n: usize, r: Vec<u8>| {
            let o = 4 * CLUSTER as usize + n * REC;
            v[o..o + REC].copy_from_slice(&r);
        };
        put(
            0,
            Rec::new(true, false, 0)
                .file_name(5, "$MFT", 3)
                .nonres(ATTR_DATA, "", &[(Some(4), 8)], 32 * REC as u64)
                .finish(),
        );
        put(
            2,
            Rec::new(true, false, 0)
                .file_name(5, "$LogFile", 3)
                .nonres(ATTR_DATA, "", &[(Some(80), 4)], 4 * CLUSTER)
                .finish(),
        );
        put(5, Rec::new(true, true, 0).file_name(5, ".", 3).finish());
        put(
            6,
            Rec::new(true, false, 0)
                .file_name(5, "$Bitmap", 3)
                .nonres(ATTR_DATA, "", &[(Some(12), 1)], 16)
                .finish(),
        );
        put(
            24,
            Rec::new(true, true, 0)
                .file_name(5, "Users", 3)
                .nonres(ATTR_INDEX_ALLOCATION, "$I30", &[(Some(20), 1)], CLUSTER)
                .finish(),
        );
        put(
            25,
            Rec::new(true, false, 0)
                .file_name(24, "big.bin", 3)
                .nonres(ATTR_DATA, "", &[(Some(30), 4), (Some(40), 2)], 6 * CLUSTER)
                .finish(),
        );
        put(
            26,
            Rec::new(true, false, 0)
                .file_name(24, "WIN32B~1.BIN", 2)
                .file_name(24, "win32.bin", 1)
                .nonres(ATTR_DATA, "", &[(Some(50), 2)], 2 * CLUSTER)
                .finish(),
        );
        put(
            27,
            Rec::new(false, false, 0)
                .file_name(24, "deleted.bin", 3)
                .nonres(ATTR_DATA, "", &[(Some(70), 2)], 2 * CLUSTER)
                .finish(),
        );
        put(
            28,
            Rec::new(true, false, 26)
                .nonres(ATTR_DATA, "", &[(Some(55), 2)], 0)
                .finish(),
        );
        put(
            29,
            Rec::new(true, false, 0)
                .file_name(24, "holes.bin", 3)
                .nonres(ATTR_DATA, "", &[(Some(60), 1), (None, 5), (Some(66), 1)], 7 * CLUSTER)
                .finish(),
        );
        // $Bitmap content at cluster 12: allocated clusters
        let bm = 12 * CLUSTER as usize;
        for c in [
            0usize, 4, 5, 6, 7, 8, 9, 10, 11, 12, 20, 30, 31, 32, 33, 40, 41, 50, 51, 55, 56, 60, 66, 80, 81, 82, 83,
        ] {
            v[bm + c / 8] |= 1 << (c % 8);
        }
        Image(v)
    }

    #[test]
    fn boot_sector_is_parsed() {
        let img = image();
        let b = parse_boot(&img.0[..512]).unwrap();
        assert_eq!(
            (b.cluster_size, b.mft_lcn, b.record_size, b.total_clusters),
            (4096, 4, 1024, 128)
        );
        assert!(parse_boot(&[0u8; 512]).is_err());
        let mut x = img.0[..512].to_vec();
        x[0x40] = 1; // 1 cluster per record
        assert_eq!(parse_boot(&x).unwrap().record_size, 4096);
    }

    #[test]
    fn run_lists_decode_with_negative_deltas_and_holes() {
        let bytes = encode_runs(&[(Some(1000), 4), (Some(200), 3), (None, 10), (Some(5_000_000), 1)]);
        let r = decode_runs(&bytes, 0).unwrap();
        assert_eq!(
            r,
            vec![
                Run {
                    vcn: 0,
                    lcn: Some(1000),
                    len: 4
                },
                Run {
                    vcn: 4,
                    lcn: Some(200),
                    len: 3
                },
                Run {
                    vcn: 7,
                    lcn: None,
                    len: 10
                },
                Run {
                    vcn: 17,
                    lcn: Some(5_000_000),
                    len: 1
                }
            ]
        );
        assert!(decode_runs(&[0x21, 0x04], 0).is_err()); // truncated
        assert!(decode_runs(&[0x11, 0x01, 0x80, 0x00], 0).is_err()); // LCN < 0
    }

    #[test]
    fn fixups_restore_the_sector_tails_and_detect_tearing() {
        let img = image();
        let o = 4 * CLUSTER as usize;
        let mut rec = img.0[o + 25 * REC..o + 26 * REC].to_vec();
        assert_eq!(&rec[510..512], &[7, 0]); // protected
        apply_fixups(&mut rec).unwrap();
        assert_ne!(&rec[510..512], &[7, 0]);
        let mut torn = img.0[o + 25 * REC..o + 26 * REC].to_vec();
        torn[1022] ^= 0xFF;
        assert!(apply_fixups(&mut torn).is_err());
    }

    #[test]
    fn records_expose_names_and_runs() {
        let img = image();
        let o = 4 * CLUSTER as usize;
        let mut rec = img.0[o + 26 * REC..o + 27 * REC].to_vec();
        apply_fixups(&mut rec).unwrap();
        let r = parse_record(26, &rec).unwrap().unwrap();
        assert_eq!(r.best_name().unwrap().name, "win32.bin"); // Win32 beats the DOS name
        assert_eq!(
            r.streams[0].runs,
            vec![Run {
                vcn: 0,
                lcn: Some(50),
                len: 2
            }]
        );
        let mut deleted = img.0[o + 27 * REC..o + 28 * REC].to_vec();
        apply_fixups(&mut deleted).unwrap();
        assert!(parse_record(27, &deleted).unwrap().is_none());
    }

    fn map(ranges: &[(u64, u64)]) -> Vec<ZoneMap> {
        map_ranges(&image(), ranges, &mut |_, _| {}).unwrap().1
    }

    fn zone(a: u64, b: u64) -> (u64, u64) {
        (a * CLUSTER, b * CLUSTER)
    }

    #[test]
    fn a_zone_inside_a_fragmented_file_finds_that_file() {
        let z = &map(&[zone(31, 33), zone(40, 42)]);
        assert_eq!(
            z[0].files,
            vec![FileHit {
                path: "\\Users\\big.bin".into(),
                kind: Kind::File,
                record: 25,
                attr: "data".into(),
                bytes: 2 * CLUSTER
            }]
        );
        assert_eq!(z[1].files[0].path, "\\Users\\big.bin");
        assert_eq!((z[0].free_bytes, z[0].other_allocated_bytes), (0, 0));
    }

    #[test]
    fn extension_records_are_credited_to_their_base_file() {
        let z = &map(&[zone(55, 57)])[0];
        assert_eq!(z.files.len(), 1);
        assert_eq!(
            (z.files[0].path.as_str(), z.files[0].record),
            ("\\Users\\win32.bin", 26)
        );
    }

    #[test]
    fn metadata_directories_and_deleted_files() {
        let z = map(&[zone(4, 12), zone(20, 21), zone(70, 72), zone(80, 84)]);
        assert_eq!(
            (z[0].files[0].path.as_str(), z[0].files[0].kind),
            ("\\$MFT", Kind::Metadata)
        );
        assert_eq!(
            (
                z[1].files[0].path.as_str(),
                z[1].files[0].kind,
                z[1].files[0].attr.as_str()
            ),
            ("\\Users", Kind::Directory, "directory index")
        );
        // the deleted file's clusters are free and nobody owns them
        assert!(z[2].files.is_empty());
        assert_eq!((z[2].free_bytes, z[2].total_bytes), (2 * CLUSTER, 2 * CLUSTER));
        assert_eq!(z[3].files[0].path, "\\$LogFile");
    }

    #[test]
    fn free_space_and_sparse_holes_are_not_attributed() {
        let z = &map(&[zone(60, 68)])[0];
        // clusters 60 and 66 belong to holes.bin; 61..66 is the hole and 67 is free
        assert_eq!(z.files.iter().map(|f| f.bytes).sum::<u64>(), 2 * CLUSTER);
        assert_eq!(z.free_bytes, 6 * CLUSTER);
        assert_eq!(z.other_allocated_bytes, 0);
    }

    #[test]
    fn allocated_clusters_nobody_claims_are_reported_as_such() {
        let mut img = image();
        // mark cluster 100 allocated in the bitmap without any file owning it
        img.0[12 * CLUSTER as usize + 100 / 8] |= 1 << (100 % 8);
        let z = &map_ranges(&img, &[zone(99, 102)], &mut |_, _| {}).unwrap().1[0];
        assert!(z.files.is_empty());
        assert_eq!((z.other_allocated_bytes, z.free_bytes), (CLUSTER, 2 * CLUSTER));
    }

    #[test]
    fn a_zone_spanning_several_files_lists_each() {
        let z = &map(&[zone(28, 62)])[0];
        let names: Vec<&str> = z.files.iter().map(|f| f.path.as_str()).collect();
        assert!(
            names.contains(&"\\Users\\big.bin")
                && names.contains(&"\\Users\\win32.bin")
                && names.contains(&"\\Users\\holes.bin"),
            "{names:?}"
        );
        // sorted by size, biggest first
        assert!(z.files.windows(2).all(|w| w[0].bytes >= w[1].bytes));
    }

    #[test]
    fn targets_merge_and_intersect() {
        let t = Targets::new(vec![(10, 20), (15, 30), (50, 60), (5, 5)]);
        assert_eq!(t.intersect(0, 100), vec![(10, 30), (50, 60)]);
        assert_eq!(t.intersect(25, 55), vec![(25, 30), (50, 55)]);
        assert!(t.intersect(30, 50).is_empty());
    }

    #[test]
    fn corrupt_volumes_are_errors_not_panics() {
        let mut img = image();
        img.0[3] = b'X';
        assert!(map_ranges(&img, &[zone(1, 2)], &mut |_, _| {}).is_err());
        let mut img = image();
        let o = 4 * CLUSTER as usize;
        img.0[o..o + 4].copy_from_slice(b"XXXX"); // $MFT record destroyed
        assert!(map_ranges(&img, &[zone(1, 2)], &mut |_, _| {}).is_err());
    }
}
