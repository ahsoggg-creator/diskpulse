//! Surface read test: sequential **read-only** pass over a disk, timing every block.
//!
//! It cannot write: the module has no write API at all. The only device constructor it uses is
//! [`crate::winapi::Device::open_read_only_unbuffered`] (`GENERIC_READ`, `FILE_FLAG_NO_BUFFERING`), the trait it reads
//! through only has `read_at`, and a unit test greps this very file for write-related identifiers.
//!
//! What the numbers mean (and do not): a slow block says the *drive* needed long to deliver those bytes (retries,
//! remapping, a weak area, a busy link). On an SSD the logical block address is not a physical cell, so "where" is only
//! a logical address. Results depend on the drive, its cache and what else uses it during the test.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::error::{Error, Result};

/// A 4096-byte aligned buffer (unbuffered I/O needs sector-aligned memory).
pub struct AlignedBuf {
    raw: Vec<u8>,
    start: usize,
    len: usize,
}

impl AlignedBuf {
    pub const ALIGN: usize = 4096;

    pub fn new(len: usize) -> AlignedBuf {
        let raw = vec![0u8; len + Self::ALIGN];
        let start = raw.as_ptr().align_offset(Self::ALIGN);
        AlignedBuf { raw, start, len }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.raw[self.start..self.start + self.len]
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.raw[self.start..self.start + self.len]
    }
}

pub const SECTOR: u64 = 4096;
pub const MIB: u64 = 1 << 20;

/// Latency buckets in milliseconds: `< 1`, `1-2`, `2-5`, ... `>= 1000`.
pub const HISTOGRAM_EDGES_MS: [f64; 10] = [1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0];

/// Where blocks are read from. Reading is the only operation there is.
pub trait BlockSource {
    fn size(&self) -> u64;
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize>;

    /// The read and how long it took, in microseconds. Overridden by tests to make timing deterministic.
    fn read_timed(&self, offset: u64, buf: &mut [u8]) -> (Result<usize>, u64) {
        let t = Instant::now();
        let r = self.read_at(offset, buf);
        (r, t.elapsed().as_micros().min(u128::from(u64::MAX)) as u64)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Params {
    /// Block size in bytes: a multiple of 4096, 1 to 8 MiB.
    pub block_bytes: u64,
    /// First byte to test (multiple of 4096).
    pub start: u64,
    /// Bytes to test from `start` (0 = to the end).
    pub len: u64,
    /// A block that took longer than this is "slow".
    pub slow_ms: f64,
    /// Stop early after this many unreadable blocks.
    pub max_bad: usize,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            block_bytes: 4 * MIB,
            start: 0,
            len: 0,
            slow_ms: 150.0,
            max_bad: 2000,
        }
    }
}

impl Params {
    pub fn validate(&self, device_size: u64) -> Result<()> {
        if !(MIB..=8 * MIB).contains(&self.block_bytes) || !self.block_bytes.is_multiple_of(SECTOR) {
            return Err(Error::Unsupported(
                "block size must be 1-8 MiB and a multiple of 4096".into(),
            ));
        }
        if !self.start.is_multiple_of(SECTOR) || self.start >= device_size {
            return Err(Error::Unsupported(
                "start offset must be sector aligned and inside the device".into(),
            ));
        }
        Ok(())
    }
}

/// One timed block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    pub offset: u64,
    pub len: u32,
    pub micros: u64,
    /// Windows error code if the block could not be read in full.
    pub error: Option<u32>,
}

/// A run of adjacent slow (or unreadable) blocks.
#[derive(Debug, Clone, PartialEq)]
pub struct Zone {
    pub start: u64,
    pub end: u64,
    pub blocks: u32,
    pub max_ms: f64,
    pub mean_ms: f64,
    pub has_errors: bool,
}

/// A sector range that could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadRange {
    pub start: u64,
    pub end: u64,
    pub error: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub blocks: u64,
    pub last_mbps: f64,
}

#[derive(Debug, Clone)]
pub struct Report {
    pub params: Params,
    pub device_size: u64,
    pub blocks: Vec<Block>,
    pub bad: Vec<BadRange>,
    pub histogram: [u64; 11],
    pub bytes_read: u64,
    pub micros_total: u64,
    pub cancelled: bool,
    /// Stopped because `max_bad` was reached.
    pub too_many_errors: bool,
}

impl Report {
    pub fn avg_mbps(&self) -> f64 {
        if self.micros_total == 0 {
            0.0
        } else {
            self.bytes_read as f64 / self.micros_total as f64
        }
    }

    pub fn zones(&self) -> Vec<Zone> {
        zones(&self.blocks, self.params.slow_ms)
    }
}

/// Index of the histogram bucket for a latency.
pub fn bucket(ms: f64) -> usize {
    HISTOGRAM_EDGES_MS
        .iter()
        .position(|e| ms < *e)
        .unwrap_or(HISTOGRAM_EDGES_MS.len())
}

/// Merges adjacent slow or failed blocks into zones.
pub fn zones(blocks: &[Block], slow_ms: f64) -> Vec<Zone> {
    let mut out: Vec<Zone> = Vec::new();
    let mut sum_ms = 0.0;
    for b in blocks {
        let ms = b.micros as f64 / 1000.0;
        if ms <= slow_ms && b.error.is_none() {
            continue;
        }
        match out.last_mut().filter(|z| z.end == b.offset) {
            Some(z) => {
                z.end = b.offset + u64::from(b.len);
                z.blocks += 1;
                z.max_ms = z.max_ms.max(ms);
                sum_ms += ms;
                z.mean_ms = sum_ms / f64::from(z.blocks);
                z.has_errors |= b.error.is_some();
            }
            None => {
                sum_ms = ms;
                out.push(Zone {
                    start: b.offset,
                    end: b.offset + u64::from(b.len),
                    blocks: 1,
                    max_ms: ms,
                    mean_ms: ms,
                    has_errors: b.error.is_some(),
                });
            }
        }
    }
    out
}

/// Speed over the disk for a chart: at most `max_points` points `(offset in bytes, MB/s)`, each the average of a group
/// of consecutive blocks (total bytes / total time of the group).
pub fn speed_series(blocks: &[Block], max_points: usize) -> Vec<(u64, f64)> {
    if blocks.is_empty() || max_points == 0 {
        return Vec::new();
    }
    let group = blocks.len().div_ceil(max_points);
    blocks
        .chunks(group)
        .filter_map(|c| {
            let (bytes, us) = c
                .iter()
                .fold((0u64, 0u64), |(b, t), x| (b + u64::from(x.len), t + x.micros));
            (us > 0).then(|| (c[0].offset, bytes as f64 / us as f64))
        })
        .collect()
}

/// Reads `[offset, offset + len)` in sectors to find exactly which sectors fail; merges neighbours.
fn locate_bad(src: &dyn BlockSource, offset: u64, len: u64, buf: &mut [u8], out: &mut Vec<BadRange>, first_error: u32) {
    let mut o = offset;
    while o < offset + len {
        let n = SECTOR.min(offset + len - o) as usize;
        if let Err(e) = src.read_at(o, &mut buf[..n]) {
            let code = match e {
                Error::Os { code, .. } => code,
                _ => first_error,
            };
            match out.last_mut().filter(|r| r.end == o && r.error == code) {
                Some(r) => r.end = o + n as u64,
                None => out.push(BadRange {
                    start: o,
                    end: o + n as u64,
                    error: code,
                }),
            }
        }
        o += n as u64;
    }
}

/// Runs the test. `cancel` is checked before every block; `progress` is called after every block.
pub fn scan(
    src: &dyn BlockSource,
    params: Params,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Result<Report> {
    let size = src.size();
    params.validate(size)?;
    let end = if params.len == 0 {
        size
    } else {
        (params.start + params.len).min(size)
    } / SECTOR
        * SECTOR;
    let total = end - params.start;
    let mut buf = AlignedBuf::new(params.block_bytes as usize);
    let mut report = Report {
        params,
        device_size: size,
        blocks: Vec::with_capacity((total / params.block_bytes + 1) as usize),
        bad: Vec::new(),
        histogram: [0; 11],
        bytes_read: 0,
        micros_total: 0,
        cancelled: false,
        too_many_errors: false,
    };
    let mut offset = params.start;
    while offset < end {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        let len = params.block_bytes.min(end - offset);
        let (r, micros) = src.read_timed(offset, &mut buf.as_mut_slice()[..len as usize]);
        let error = match r {
            Ok(n) if n as u64 >= len => None,
            Ok(_) => Some(0),
            Err(Error::Os { code, .. }) => Some(code),
            Err(_) => Some(0),
        };
        if let Some(code) = error {
            locate_bad(src, offset, len, buf.as_mut_slice(), &mut report.bad, code);
        } else {
            report.bytes_read += len;
        }
        report.micros_total += micros;
        report.histogram[bucket(micros as f64 / 1000.0)] += 1;
        report.blocks.push(Block {
            offset,
            len: len as u32,
            micros,
            error,
        });
        progress(Progress {
            done_bytes: offset + len - params.start,
            total_bytes: total,
            blocks: report.blocks.len() as u64,
            last_mbps: if micros > 0 { len as f64 / micros as f64 } else { 0.0 },
        });
        if report.bad.len() >= params.max_bad {
            report.too_many_errors = true;
            break;
        }
        offset += len;
    }
    Ok(report)
}

// ---- the Windows device -----------------------------------------------------------------------------------------

/// A physical drive opened read-only.
#[cfg(windows)]
pub struct ReadOnlyDisk {
    dev: crate::winapi::Device,
    size: u64,
}

#[cfg(windows)]
impl ReadOnlyDisk {
    pub fn open(number: u32, size: u64) -> Result<ReadOnlyDisk> {
        Ok(ReadOnlyDisk {
            dev: crate::winapi::Device::open_read_only_unbuffered(&format!(r"\\.\PhysicalDrive{number}"))?,
            size,
        })
    }
}

#[cfg(windows)]
impl BlockSource for ReadOnlyDisk {
    fn size(&self) -> u64 {
        self.size
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        self.dev.read_at(offset, buf)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// A fake disk: `size` bytes, per-block synthetic latency, and a set of unreadable sector offsets.
    struct Fake {
        size: u64,
        slow_from: u64,
        slow_to: u64,
        bad_sectors: Vec<u64>,
        reads: RefCell<u64>,
    }

    impl Fake {
        fn new(size: u64) -> Fake {
            Fake {
                size,
                slow_from: u64::MAX,
                slow_to: u64::MAX,
                bad_sectors: vec![],
                reads: RefCell::new(0),
            }
        }
    }

    impl BlockSource for Fake {
        fn size(&self) -> u64 {
            self.size
        }

        fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
            *self.reads.borrow_mut() += 1;
            let end = offset + buf.len() as u64;
            if self.bad_sectors.iter().any(|s| *s >= offset && *s < end) {
                return Err(Error::Os {
                    op: "ReadFile",
                    code: 23,
                });
            }
            Ok(buf.len())
        }

        fn read_timed(&self, offset: u64, buf: &mut [u8]) -> (Result<usize>, u64) {
            let r = self.read_at(offset, buf);
            let slow = offset >= self.slow_from && offset < self.slow_to;
            (r, if slow { 400_000 } else { 8_000 }) // 400 ms vs 8 ms
        }
    }

    fn run(src: &Fake, p: Params) -> Report {
        scan(src, p, &AtomicBool::new(false), &mut |_| {}).unwrap()
    }

    #[test]
    fn healthy_disk_has_no_zones_and_counts_every_byte() {
        let src = Fake::new(40 * MIB);
        let r = run(
            &src,
            Params {
                block_bytes: 4 * MIB,
                ..Params::default()
            },
        );
        assert_eq!(r.blocks.len(), 10);
        assert_eq!(r.bytes_read, 40 * MIB);
        assert!(r.zones().is_empty() && r.bad.is_empty());
        assert_eq!(r.histogram[bucket(8.0)], 10);
        // 4 MiB in 8 ms
        assert!((r.avg_mbps() - 4.0 * 1048576.0 / 8000.0).abs() < 1.0);
    }

    #[test]
    fn slow_blocks_are_merged_into_zones() {
        let mut src = Fake::new(80 * MIB);
        src.slow_from = 8 * MIB;
        src.slow_to = 20 * MIB; // blocks 2,3,4 (4 MiB each)
        let r = run(&src, Params::default());
        let z = r.zones();
        assert_eq!(z.len(), 1);
        assert_eq!((z[0].start, z[0].end, z[0].blocks), (8 * MIB, 20 * MIB, 3));
        assert!((z[0].max_ms - 400.0).abs() < 1e-9 && !z[0].has_errors);
        // a different threshold changes the verdict
        assert!(zones(&r.blocks, 500.0).is_empty());
    }

    #[test]
    fn unreadable_blocks_are_located_to_the_sector_and_listed_separately() {
        let mut src = Fake::new(16 * MIB);
        src.bad_sectors = vec![5 * MIB, 5 * MIB + 4096, 11 * MIB + 8192];
        let r = run(
            &src,
            Params {
                block_bytes: MIB,
                ..Params::default()
            },
        );
        assert_eq!(
            r.bad,
            vec![
                BadRange {
                    start: 5 * MIB,
                    end: 5 * MIB + 8192,
                    error: 23
                },
                BadRange {
                    start: 11 * MIB + 8192,
                    end: 11 * MIB + 12288,
                    error: 23
                }
            ]
        );
        // the failed blocks are zones too, flagged as errors, and are not counted as read
        let z = r.zones();
        assert_eq!(z.len(), 2);
        assert!(z.iter().all(|z| z.has_errors));
        assert_eq!(r.bytes_read, 14 * MIB);
    }

    #[test]
    fn cancel_stops_before_the_next_block() {
        let src = Fake::new(40 * MIB);
        let cancel = AtomicBool::new(false);
        let mut seen = 0;
        let r = scan(&src, Params::default(), &cancel, &mut |p| {
            seen += 1;
            if p.blocks == 3 {
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .unwrap();
        assert!(r.cancelled);
        assert_eq!((r.blocks.len(), seen), (3, 3));
    }

    #[test]
    fn range_and_tail_are_respected() {
        let src = Fake::new(100 * MIB + 4096 * 3);
        let r = run(
            &src,
            Params {
                block_bytes: 8 * MIB,
                start: 96 * MIB,
                len: 0,
                ..Params::default()
            },
        );
        assert_eq!(r.blocks.len(), 1);
        assert_eq!(u64::from(r.blocks[0].len), 4 * MIB + 4096 * 3);
        let r = run(
            &src,
            Params {
                start: MIB,
                len: 6 * MIB,
                ..Params::default()
            },
        );
        assert_eq!(r.bytes_read, 6 * MIB);
    }

    #[test]
    fn stops_after_too_many_unreadable_blocks() {
        let mut src = Fake::new(64 * MIB);
        src.bad_sectors = (0..64).map(|i| i * MIB).collect();
        let r = run(
            &src,
            Params {
                block_bytes: MIB,
                max_bad: 5,
                ..Params::default()
            },
        );
        assert!(r.too_many_errors);
        assert_eq!(r.bad.len(), 5);
    }

    #[test]
    fn invalid_parameters_are_rejected() {
        let src = Fake::new(10 * MIB);
        let bad = |p: Params| scan(&src, p, &AtomicBool::new(false), &mut |_| {}).is_err();
        assert!(bad(Params {
            block_bytes: 512 * 1024,
            ..Params::default()
        }));
        assert!(bad(Params {
            block_bytes: 16 * MIB,
            ..Params::default()
        }));
        assert!(bad(Params {
            block_bytes: MIB + 1,
            ..Params::default()
        }));
        assert!(bad(Params {
            start: 100,
            ..Params::default()
        }));
        assert!(bad(Params {
            start: 10 * MIB,
            ..Params::default()
        }));
    }

    #[test]
    fn speed_series_is_decimated() {
        let blocks: Vec<Block> = (0..1000)
            .map(|i| Block {
                offset: i * MIB,
                len: MIB as u32,
                micros: 10_000,
                error: None,
            })
            .collect();
        let s = speed_series(&blocks, 100);
        assert_eq!(s.len(), 100);
        assert!((s[0].1 - 104.8576).abs() < 1e-3);
        assert!(speed_series(&[], 10).is_empty());
    }

    #[test]
    fn histogram_buckets() {
        assert_eq!(
            (bucket(0.5), bucket(1.0), bucket(7.0), bucket(999.0), bucket(5000.0)),
            (0, 1, 3, 9, 10)
        );
    }

    /// The read-only promise is checked in the source: this module must not contain anything that can modify a device.
    #[test]
    fn this_module_cannot_write() {
        let src = include_str!("surface.rs");
        let production = src.split("#[cfg(test)]").next().unwrap();
        for forbidden in [
            "GENERIC_WRITE",
            "WriteFile",
            "FILE_WRITE",
            "write_at",
            "IOCTL_DISK_",
            "FSCTL_",
            "DeviceIoControl",
            "SetFilePointer",
            "open_smart",
            "Device::open(",
        ] {
            assert!(
                !production.contains(forbidden),
                "surface.rs must not mention {forbidden}"
            );
        }
        assert!(production.contains("open_read_only_unbuffered"));
        // and the only constructor that module is allowed to use opens GENERIC_READ only
        let winapi = include_str!("winapi.rs");
        let ctor = winapi
            .split("pub fn open_read_only_unbuffered")
            .nth(1)
            .unwrap()
            .split('}')
            .next()
            .unwrap();
        assert!(
            ctor.contains("GENERIC_READ") && !ctor.contains("GENERIC_WRITE"),
            "{ctor}"
        );
    }
}
