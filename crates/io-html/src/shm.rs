//! Shared frames of one HTML input: [`SLOTS`] BGRA pictures in a memory-mapped file, written
//! by `vma-html`, read by the application (ADR 0009 § 3).
//!
//! Ownership of each slot goes with the messages of [`crate::protocol`]: `vma-html` writes a
//! slot it owns then sends `Frame`, after which only the application reads it, until it sends
//! `Release`. No slot is ever written and read at the same time; the socket's write → read
//! orders the memory accesses of the two processes.
//!
//! The file lives in `/dev/shm` under Linux (memory, never written to disk), in the temporary
//! folder elsewhere. The application creates it and removes it when the input closes; under
//! Linux it is removed as soon as `vma-html` has mapped it, so a crash leaves nothing behind.

// Mapping a file is `unsafe` in `memmap2` (another process may change or truncate it): the
// two `map_mut` calls below are justified there.
#![allow(unsafe_code)]

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use memmap2::MmapMut;

use crate::protocol::SLOTS;

/// The slots of one input, mapped.
#[derive(Debug)]
pub struct SharedFrames {
    map: MmapMut,
    frame_bytes: usize,
    path: PathBuf,
    /// Created here: removed on drop.
    owner: bool,
}

/// Where the application creates shared frames.
pub fn default_dir() -> PathBuf {
    let shm = Path::new("/dev/shm");
    if cfg!(target_os = "linux") && shm.is_dir() {
        shm.to_path_buf()
    } else {
        std::env::temp_dir()
    }
}

impl SharedFrames {
    /// Bytes of one BGRA picture of `width` × `height`.
    pub fn frame_bytes(width: u32, height: u32) -> usize {
        (width as usize) * (height as usize) * 4
    }

    /// Creates (or replaces) the file at `path`, sized for [`SLOTS`] pictures, and maps it.
    pub fn create(path: &Path, frame_bytes: usize) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        file.set_len((frame_bytes * SLOTS) as u64)?;
        Self::map(file, path, frame_bytes, true)
    }

    /// Maps the file the application created (`vma-html`). Refused if its size is not
    /// [`SLOTS`] pictures of `frame_bytes`.
    pub fn open(path: &Path, frame_bytes: usize) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        if file.metadata()?.len() != (frame_bytes * SLOTS) as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "shared frames of an unexpected size",
            ));
        }
        Self::map(file, path, frame_bytes, false)
    }

    fn map(file: File, path: &Path, frame_bytes: usize, owner: bool) -> io::Result<Self> {
        if frame_bytes == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty picture"));
        }
        // SAFETY: the file is private to VMA (created by the application with a name of its
        // own, opened by its `vma-html`); its size was set (or checked) just above and neither
        // process changes it afterwards. The two processes write disjoint slots at any time
        // (ownership by the protocol, see the module), so the slices handed out below are
        // never modified behind their borrows.
        let map = unsafe { MmapMut::map_mut(&file)? };
        Ok(Self {
            map,
            frame_bytes,
            path: path.to_path_buf(),
            owner,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The picture in `slot`; `None` for a slot out of range.
    pub fn slot(&self, slot: u8) -> Option<&[u8]> {
        let start = usize::from(slot).checked_mul(self.frame_bytes)?;
        if usize::from(slot) >= SLOTS {
            return None;
        }
        self.map.get(start..start + self.frame_bytes)
    }

    /// The picture in `slot`, to write it.
    pub fn slot_mut(&mut self, slot: u8) -> Option<&mut [u8]> {
        let start = usize::from(slot).checked_mul(self.frame_bytes)?;
        if usize::from(slot) >= SLOTS {
            return None;
        }
        self.map.get_mut(start..start + self.frame_bytes)
    }

    /// Removes the file now (Linux: once `vma-html` has mapped it); the mapping stays valid.
    pub fn unlink(&mut self) {
        if self.owner && std::fs::remove_file(&self.path).is_ok() {
            self.owner = false;
        }
    }
}

impl Drop for SharedFrames {
    fn drop(&mut self) {
        if self.owner {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        default_dir().join(format!("vma-html-test-{}-{name}", std::process::id()))
    }

    #[test]
    fn what_one_side_writes_the_other_reads() {
        let path = temp_path("rw");
        let bytes = SharedFrames::frame_bytes(4, 2);
        let app = SharedFrames::create(&path, bytes).unwrap();
        let mut helper = SharedFrames::open(&path, bytes).unwrap();
        helper.slot_mut(2).unwrap().fill(0xab);
        helper.slot_mut(0).unwrap()[0] = 1;
        assert!(app.slot(2).unwrap().iter().all(|&b| b == 0xab));
        assert_eq!(app.slot(0).unwrap()[0], 1);
        assert_eq!(app.slot(1).unwrap(), &[0; 32][..]);
        assert!(app.slot(3).is_none() && helper.slot(255).is_none());
        drop(helper);
        drop(app);
        assert!(!path.exists(), "removed by its creator");
    }

    #[test]
    fn a_file_of_another_size_is_refused() {
        let path = temp_path("size");
        let _app = SharedFrames::create(&path, 64).unwrap();
        assert!(SharedFrames::open(&path, 128).is_err());
        assert!(SharedFrames::open(&path, 64).is_ok());
    }

    #[test]
    fn an_unlinked_file_stays_mapped() {
        let path = temp_path("unlink");
        let mut app = SharedFrames::create(&path, 16).unwrap();
        let mut helper = SharedFrames::open(&path, 16).unwrap();
        app.unlink();
        if cfg!(unix) {
            assert!(!path.exists());
        }
        helper.slot_mut(1).unwrap()[3] = 9;
        assert_eq!(app.slot(1).unwrap()[3], 9);
    }
}
