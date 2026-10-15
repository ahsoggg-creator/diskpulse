//! Ties surface-scan results to files: a byte range of a *physical disk* becomes a cluster range of the *volume* that
//! contains it (partition offset subtracted), and [`crate::ntfs`] says which files own those clusters.

use crate::disks::VolumeInfo;
use crate::error::Result;
use crate::ntfs::{self, VolumeRead, ZoneMap};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The range lies on an NTFS volume and was looked up.
    Mapped { volume: String, zone: ZoneMap },
    /// The range is not inside any mounted volume (partition table, an unmounted or hidden partition, free disk space).
    NoVolume,
    /// A volume with a file system DiskPulse cannot look into (only NTFS is supported).
    UnsupportedFs { volume: String, fs: String },
    /// A volume made of several disk extents (spanned / striped) is not supported.
    MultiExtent { volume: String },
    /// Reading the volume or its MFT failed (for example without administrator rights).
    Failed { volume: String, error: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    /// Byte range on the physical disk.
    pub start: u64,
    pub end: u64,
    pub outcome: Outcome,
}

/// Splits `ranges` at volume borders and finds the volume of each piece.
fn assign<'a>(disk: u32, ranges: &[(u64, u64)], volumes: &'a [VolumeInfo]) -> Vec<(u64, u64, Option<&'a VolumeInfo>)> {
    let mut out = Vec::new();
    for &(a, b) in ranges {
        let mut cur = a;
        while cur < b {
            let hit = volumes
                .iter()
                .flat_map(|v| v.extents.iter().map(move |e| (v, e)))
                .find(|(_, e)| e.disk == disk && cur >= e.start && cur < e.start + e.len);
            match hit {
                Some((v, e)) => {
                    let end = b.min(e.start + e.len);
                    out.push((cur, end, Some(v)));
                    cur = end;
                }
                None => {
                    // up to the next extent start on this disk, or the end of the range
                    let next = volumes
                        .iter()
                        .flat_map(|v| &v.extents)
                        .filter(|e| e.disk == disk && e.start > cur)
                        .map(|e| e.start)
                        .min()
                        .unwrap_or(b)
                        .min(b);
                    out.push((cur, next, None));
                    cur = next;
                }
            }
        }
    }
    out
}

/// `open(letter)` opens a volume for reading (read-only).
pub fn attribute(
    disk: u32,
    ranges: &[(u64, u64)],
    volumes: &[VolumeInfo],
    open: &dyn Fn(&str) -> Result<Box<dyn VolumeRead>>,
    progress: &mut dyn FnMut(u64, u64),
) -> Vec<Piece> {
    let assigned = assign(disk, ranges, volumes);
    let mut pieces: Vec<Option<Piece>> = vec![None; assigned.len()];
    for (i, (a, b, v)) in assigned.iter().enumerate() {
        if v.is_none() {
            pieces[i] = Some(Piece {
                start: *a,
                end: *b,
                outcome: Outcome::NoVolume,
            });
        }
    }
    // one pass over the MFT per volume, however many ranges it holds
    let mut letters: Vec<&VolumeInfo> = assigned.iter().filter_map(|(_, _, v)| *v).collect();
    letters.sort_by(|x, y| x.letter.cmp(&y.letter));
    letters.dedup_by(|x, y| x.letter == y.letter);
    for vol in letters {
        let idx: Vec<usize> = assigned
            .iter()
            .enumerate()
            .filter(|(_, (_, _, v))| v.is_some_and(|v| v.letter == vol.letter))
            .map(|(i, _)| i)
            .collect();
        let fail = |pieces: &mut Vec<Option<Piece>>, outcome: Outcome| {
            for &i in &idx {
                pieces[i] = Some(Piece {
                    start: assigned[i].0,
                    end: assigned[i].1,
                    outcome: outcome.clone(),
                });
            }
        };
        if !vol.fs.eq_ignore_ascii_case("NTFS") {
            fail(
                &mut pieces,
                Outcome::UnsupportedFs {
                    volume: vol.letter.clone(),
                    fs: vol.fs.clone(),
                },
            );
            continue;
        }
        if vol.extents.len() != 1 {
            fail(
                &mut pieces,
                Outcome::MultiExtent {
                    volume: vol.letter.clone(),
                },
            );
            continue;
        }
        let ext = vol.extents[0];
        let vranges: Vec<(u64, u64)> = idx
            .iter()
            .map(|&i| (assigned[i].0 - ext.start, assigned[i].1 - ext.start))
            .collect();
        let res = open(&vol.letter).and_then(|v| ntfs::map_ranges(v.as_ref(), &vranges, progress));
        match res {
            Ok((_, zones)) => {
                for (&i, zone) in idx.iter().zip(zones) {
                    pieces[i] = Some(Piece {
                        start: assigned[i].0,
                        end: assigned[i].1,
                        outcome: Outcome::Mapped {
                            volume: vol.letter.clone(),
                            zone,
                        },
                    });
                }
            }
            Err(e) => fail(
                &mut pieces,
                Outcome::Failed {
                    volume: vol.letter.clone(),
                    error: e.to_string(),
                },
            ),
        }
    }
    pieces.into_iter().flatten().collect()
}

#[cfg(windows)]
pub fn open_volume(letter: &str) -> Result<Box<dyn VolumeRead>> {
    Ok(Box::new(ntfs::WinVolume::open(letter)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disks::Extent;
    use crate::ntfs::tests::{CLUSTER, image};

    const PART: u64 = 1 << 20; // the volume starts 1 MiB into the disk

    fn vol(letter: &str, fs: &str, extents: Vec<Extent>) -> VolumeInfo {
        VolumeInfo {
            letter: letter.into(),
            fs: fs.into(),
            extents,
        }
    }

    fn ntfs_volume() -> VolumeInfo {
        vol(
            "C:",
            "NTFS",
            vec![Extent {
                disk: 3,
                start: PART,
                len: 128 * CLUSTER,
            }],
        )
    }

    fn opener(letter: &str) -> Result<Box<dyn VolumeRead>> {
        assert_eq!(letter, "C:");
        Ok(Box::new(image()))
    }

    #[test]
    fn disk_offsets_become_volume_clusters_after_subtracting_the_partition_start() {
        // disk bytes of clusters 31..33 of the volume
        let r = [(PART + 31 * CLUSTER, PART + 33 * CLUSTER)];
        let p = attribute(3, &r, &[ntfs_volume()], &opener, &mut |_, _| {});
        assert_eq!(p.len(), 1);
        match &p[0].outcome {
            Outcome::Mapped { volume, zone } => {
                assert_eq!(volume, "C:");
                assert_eq!(zone.files[0].path, "\\Users\\big.bin");
            }
            o => panic!("{o:?}"),
        }
        assert_eq!((p[0].start, p[0].end), (r[0].0, r[0].1));
    }

    #[test]
    fn ranges_outside_volumes_say_so_and_straddling_ranges_are_split() {
        // starts in the gap before the partition and runs into the volume
        let r = [(PART - 8192, PART + 2 * CLUSTER)];
        let p = attribute(3, &r, &[ntfs_volume()], &opener, &mut |_, _| {});
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].outcome, Outcome::NoVolume);
        assert_eq!((p[0].start, p[0].end), (PART - 8192, PART));
        assert!(matches!(p[1].outcome, Outcome::Mapped { .. }));
        assert_eq!(p[1].start, PART);
    }

    #[test]
    fn other_disks_and_other_file_systems_are_not_looked_into() {
        let other_disk = vol(
            "D:",
            "NTFS",
            vec![Extent {
                disk: 9,
                start: 0,
                len: 1 << 30,
            }],
        );
        let fat = vol(
            "E:",
            "exFAT",
            vec![Extent {
                disk: 3,
                start: 1 << 28,
                len: 1 << 28,
            }],
        );
        let spanned = vol(
            "F:",
            "NTFS",
            vec![
                Extent {
                    disk: 3,
                    start: 1 << 30,
                    len: 1 << 20,
                },
                Extent {
                    disk: 4,
                    start: 0,
                    len: 1 << 20,
                },
            ],
        );
        let vols = [other_disk, fat, spanned];
        let r = [(100, 4096), (1 << 28, (1 << 28) + 4096), (1 << 30, (1 << 30) + 4096)];
        let p = attribute(3, &r, &vols, &opener, &mut |_, _| {});
        assert_eq!(p[0].outcome, Outcome::NoVolume);
        assert_eq!(
            p[1].outcome,
            Outcome::UnsupportedFs {
                volume: "E:".into(),
                fs: "exFAT".into()
            }
        );
        assert_eq!(p[2].outcome, Outcome::MultiExtent { volume: "F:".into() });
    }

    #[test]
    fn a_failing_volume_is_reported_per_piece_without_aborting() {
        let r = [
            (PART + CLUSTER, PART + 2 * CLUSTER),
            (PART + 31 * CLUSTER, PART + 32 * CLUSTER),
        ];
        let failing = |_: &str| -> Result<Box<dyn VolumeRead>> {
            Err(crate::Error::Os {
                op: "CreateFile",
                code: 5,
            })
        };
        let p = attribute(3, &r, &[ntfs_volume()], &failing, &mut |_, _| {});
        assert_eq!(p.len(), 2);
        assert!(
            p.iter()
                .all(|x| matches!(&x.outcome, Outcome::Failed { error, .. } if error.contains("5")))
        );
    }

    #[test]
    fn one_volume_is_opened_once_for_all_its_ranges() {
        let calls = std::cell::Cell::new(0);
        let counting = |l: &str| {
            calls.set(calls.get() + 1);
            opener(l)
        };
        let r = [
            (PART + 31 * CLUSTER, PART + 32 * CLUSTER),
            (PART + 55 * CLUSTER, PART + 56 * CLUSTER),
            (PART + 20 * CLUSTER, PART + 21 * CLUSTER),
        ];
        let p = attribute(3, &r, &[ntfs_volume()], &counting, &mut |_, _| {});
        assert_eq!((calls.get(), p.len()), (1, 3));
        let names: Vec<String> = p
            .iter()
            .map(|x| match &x.outcome {
                Outcome::Mapped { zone, .. } => zone.files[0].path.clone(),
                o => panic!("{o:?}"),
            })
            .collect();
        assert_eq!(names, vec!["\\Users\\big.bin", "\\Users\\win32.bin", "\\Users"]);
    }
}
