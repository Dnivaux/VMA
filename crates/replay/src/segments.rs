//! A per-input recording on disk (ADR 0015 § 4): in its folder, segments `seg-NNNNNN.h264`
//! (access units end to end) and `seg-NNNNNN.idx` (one 24-byte entry per picture: instant on
//! the shared clock in ns, offset in the segment, length — little-endian, then 4 spare bytes).
//! The writer appends the picture, then its entry: a reader that sees an entry finds its bytes.

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};

/// Bytes of one index entry.
pub const ENTRY_BYTES: u64 = 24;

/// A picture of the recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// Instant on the shared clock (ns).
    pub at_ns: u64,
    pub segment: u64,
    pub offset: u64,
    pub len: u32,
}

/// Why a recording stopped.
#[derive(Debug, thiserror::Error)]
pub enum WriterError {
    #[error("{path} : écriture impossible ({source})")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

fn data_path(dir: &Path, segment: u64) -> PathBuf {
    dir.join(format!("seg-{segment:06}.h264"))
}

fn index_path(dir: &Path, segment: u64) -> PathBuf {
    dir.join(format!("seg-{segment:06}.idx"))
}

/// Segment numbers present in `dir`, in order.
fn segments_in(dir: &Path) -> Vec<u64> {
    let mut found: Vec<u64> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_str()?;
            name.strip_prefix("seg-")?
                .strip_suffix(".idx")?
                .parse()
                .ok()
        })
        .collect();
    found.sort_unstable();
    found
}

struct Open {
    segment: u64,
    start_ns: u64,
    data: File,
    index: File,
    offset: u64,
}

/// Writes one input's recording.
pub struct SegmentWriter {
    dir: PathBuf,
    segment_ns: u64,
    /// Segments wholly older than this before the newest picture are deleted (`None`: kept).
    keep_ns: Option<u64>,
    open: Option<Open>,
    next_segment: u64,
}

impl std::fmt::Debug for SegmentWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SegmentWriter")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

impl SegmentWriter {
    /// In `dir` (created; a recording already there is continued after its last segment).
    pub fn new(dir: &Path, segment_ns: u64, keep_ns: Option<u64>) -> Result<Self, WriterError> {
        std::fs::create_dir_all(dir).map_err(|source| WriterError::Io {
            path: dir.to_owned(),
            source,
        })?;
        let next_segment = segments_in(dir).last().map_or(1, |s| s + 1);
        Ok(Self {
            dir: dir.to_owned(),
            segment_ns: segment_ns.max(1),
            keep_ns,
            open: None,
            next_segment,
        })
    }

    /// Appends one access unit taken at `at_ns`. A new segment starts every `segment_ns`.
    pub fn write(&mut self, at_ns: u64, unit: &[u8]) -> Result<(), WriterError> {
        let roll = self
            .open
            .as_ref()
            .is_none_or(|o| at_ns.saturating_sub(o.start_ns) >= self.segment_ns);
        if roll {
            let segment = self.next_segment;
            self.next_segment += 1;
            let create = |path: PathBuf| {
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .map_err(|source| WriterError::Io { path, source })
            };
            self.open = Some(Open {
                segment,
                start_ns: at_ns,
                data: create(data_path(&self.dir, segment))?,
                index: create(index_path(&self.dir, segment))?,
                offset: 0,
            });
            self.prune(at_ns);
        }
        let dir = self.dir.clone();
        let Some(open) = &mut self.open else {
            return Ok(());
        };
        let len = u32::try_from(unit.len()).unwrap_or(u32::MAX);
        open.data
            .write_all(unit)
            .map_err(|source| WriterError::Io {
                path: data_path(&dir, open.segment),
                source,
            })?;
        let mut entry = Vec::with_capacity(ENTRY_BYTES as usize);
        entry.extend_from_slice(&at_ns.to_le_bytes());
        entry.extend_from_slice(&open.offset.to_le_bytes());
        entry.extend_from_slice(&len.to_le_bytes());
        entry.extend_from_slice(&[0; 4]);
        open.index
            .write_all(&entry)
            .map_err(|source| WriterError::Io {
                path: index_path(&dir, open.segment),
                source,
            })?;
        open.offset += u64::from(len);
        Ok(())
    }

    /// Deletes the segments ended more than `keep_ns` before `now_ns` (not the open one).
    fn prune(&mut self, now_ns: u64) {
        let Some(keep) = self.keep_ns else {
            return;
        };
        let open = self.open.as_ref().map(|o| o.segment);
        let segments = segments_in(&self.dir);
        // A segment ends where the next one starts.
        for pair in segments.windows(2) {
            let [segment, next] = pair else {
                continue;
            };
            if Some(*segment) == open {
                continue;
            }
            let next_start = first_instant(&self.dir, *next);
            if next_start.is_some_and(|t| now_ns.saturating_sub(t) > keep) {
                for path in [
                    data_path(&self.dir, *segment),
                    index_path(&self.dir, *segment),
                ] {
                    if let Err(e) = std::fs::remove_file(&path) {
                        tracing::warn!(path = %path.display(), error = %e, "old segment not deleted");
                    }
                }
            }
        }
    }
}

fn first_instant(dir: &Path, segment: u64) -> Option<u64> {
    let mut file = File::open(index_path(dir, segment)).ok()?;
    let mut bytes = [0u8; 8];
    file.read_exact(&mut bytes).ok()?;
    Some(u64::from_le_bytes(bytes))
}

/// Reads a recording, also while it is written: [`Self::refresh`] adds the new pictures.
#[derive(Debug)]
pub struct SegmentReader {
    dir: PathBuf,
    entries: Vec<Entry>,
    /// Entries read so far from the last segment seen.
    last: Option<(u64, u64)>,
}

impl SegmentReader {
    pub fn open(dir: &Path) -> Self {
        let mut reader = Self {
            dir: dir.to_owned(),
            entries: Vec::new(),
            last: None,
        };
        reader.refresh();
        reader
    }

    /// Reads the entries written since the last call; drops those of deleted segments.
    pub fn refresh(&mut self) {
        let segments = segments_in(&self.dir);
        let oldest = segments.first().copied().unwrap_or(u64::MAX);
        self.entries.retain(|e| e.segment >= oldest);
        for segment in segments {
            let skip = match self.last {
                Some((s, _)) if segment < s => continue,
                Some((s, read)) if segment == s => read,
                _ => 0,
            };
            let Ok(mut file) = File::open(index_path(&self.dir, segment)) else {
                continue;
            };
            if file.seek(SeekFrom::Start(skip * ENTRY_BYTES)).is_err() {
                continue;
            }
            let mut bytes = Vec::new();
            if file.read_to_end(&mut bytes).is_err() {
                continue;
            }
            let mut read = skip;
            let (chunks, _) = bytes.as_chunks::<{ ENTRY_BYTES as usize }>();
            for chunk in chunks {
                let word = |at: usize| {
                    chunk
                        .get(at..at + 8)
                        .and_then(|b| b.try_into().ok())
                        .map_or(0, u64::from_le_bytes)
                };
                let len = chunk
                    .get(16..20)
                    .and_then(|b| b.try_into().ok())
                    .map_or(0, u32::from_le_bytes);
                self.entries.push(Entry {
                    at_ns: word(0),
                    segment,
                    offset: word(8),
                    len,
                });
                read += 1;
            }
            self.last = Some((segment, read));
        }
    }

    /// Every picture known, in recording order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The picture taken closest to `at_ns`.
    pub fn nearest(&self, at_ns: u64) -> Option<&Entry> {
        let after = self.entries.partition_point(|e| e.at_ns < at_ns);
        let before = after.checked_sub(1).and_then(|i| self.entries.get(i));
        match (before, self.entries.get(after)) {
            (Some(b), Some(a)) => Some(if at_ns - b.at_ns <= a.at_ns - at_ns {
                b
            } else {
                a
            }),
            (b, a) => b.or(a),
        }
    }

    /// The bytes of a picture.
    pub fn read(&self, entry: &Entry) -> std::io::Result<Vec<u8>> {
        let mut file = File::open(data_path(&self.dir, entry.segment))?;
        file.seek(SeekFrom::Start(entry.offset))?;
        let mut bytes = vec![0; entry.len as usize];
        file.read_exact(&mut bytes)?;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("vma-replay-{name}-{}-{nanos}", std::process::id()))
    }

    const MS: u64 = 1_000_000;

    /// ADR 0015 § 4: pictures read back by instant while the recording goes on, segments every
    /// 2 s, the oldest deleted past the time kept.
    #[test]
    fn written_pictures_are_read_back_while_recording() {
        let dir = folder("segments");
        let mut writer = SegmentWriter::new(&dir, 2_000 * MS, Some(5_000 * MS)).unwrap();
        let picture = |n: u64| format!("picture {n}").into_bytes();
        for n in 0..100 {
            writer
                .write(1_000_000_000 + n * 20 * MS, &picture(n))
                .unwrap();
        }
        let mut reader = SegmentReader::open(&dir);
        assert_eq!(reader.entries().len(), 100);
        assert_eq!(
            segments_in(&dir).len(),
            1,
            "2 s per segment: 100 pictures at 50p"
        );
        let e = *reader
            .nearest(1_000_000_000 + 41 * 20 * MS + 3 * MS)
            .unwrap();
        assert_eq!(reader.read(&e).unwrap(), picture(41));

        for n in 100..600 {
            writer
                .write(1_000_000_000 + n * 20 * MS, &picture(n))
                .unwrap();
        }
        reader.refresh();
        let first = reader.entries().first().unwrap().at_ns;
        let last = reader.entries().last().unwrap().at_ns;
        assert_eq!(last, 1_000_000_000 + 599 * 20 * MS);
        // Whole segments go: the time kept, plus up to two segments.
        assert!(
            last - first <= 9_000 * MS,
            "older segments deleted: {} ms kept",
            (last - first) / MS
        );
        assert!(last - first >= 5_000 * MS, "the time kept is there");
        let e = *reader.nearest(last).unwrap();
        assert_eq!(reader.read(&e).unwrap(), picture(599));
        // A new writer continues after the last segment.
        let before = segments_in(&dir).last().copied().unwrap();
        let mut again = SegmentWriter::new(&dir, 2_000 * MS, None).unwrap();
        again.write(last + 20 * MS, &picture(600)).unwrap();
        assert_eq!(segments_in(&dir).last().copied(), Some(before + 1));
        reader.refresh();
        assert_eq!(
            reader.read(reader.entries().last().unwrap()).unwrap(),
            picture(600)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nearest_picks_the_closest_instant() {
        let dir = folder("nearest");
        let mut writer = SegmentWriter::new(&dir, 2_000 * MS, None).unwrap();
        for at in [100, 140, 180] {
            writer.write(at * MS, b"x").unwrap();
        }
        let reader = SegmentReader::open(&dir);
        let at = |ms: u64| reader.nearest(ms * MS).unwrap().at_ns / MS;
        assert_eq!((at(0), at(119), at(121), at(500)), (100, 100, 140, 180));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
