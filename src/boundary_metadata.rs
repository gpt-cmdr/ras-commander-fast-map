//! Bounded, byte-preserving reads of fixed-width boundary metadata rows.

use crate::MapError;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) struct RowFile {
    file: Mutex<File>,
    rows: usize,
    row_bytes: usize,
    expected_len: u64,
    bytes_read: AtomicU64,
    rows_read: AtomicU64,
}

impl RowFile {
    pub(crate) fn open(path: &Path, rows: usize, row_bytes: usize) -> Result<Self, MapError> {
        let expected_len = rows
            .checked_mul(row_bytes)
            .and_then(|n| u64::try_from(n).ok())
            .ok_or_else(|| MapError::Invalid("metadata row shape overflows file length".into()))?;
        let file = File::open(path)?;
        Self::check_length(&file, expected_len)?;
        Ok(Self {
            file: Mutex::new(file),
            rows,
            row_bytes,
            expected_len,
            bytes_read: AtomicU64::new(0),
            rows_read: AtomicU64::new(0),
        })
    }

    fn check_length(file: &File, expected_len: u64) -> Result<(), MapError> {
        let actual = file.metadata()?.len();
        if actual != expected_len {
            return Err(MapError::Invalid(format!(
                "metadata file length changed or does not match row shape: expected {expected_len}, got {actual}"
            )));
        }
        Ok(())
    }

    pub(crate) fn read(&self, row: usize) -> Result<Vec<u8>, MapError> {
        if row >= self.rows {
            return Err(MapError::Invalid(format!(
                "metadata row {row} is outside {} rows",
                self.rows
            )));
        }
        let offset = row
            .checked_mul(self.row_bytes)
            .and_then(|n| u64::try_from(n).ok())
            .ok_or_else(|| MapError::Invalid("metadata row offset overflow".into()))?;
        let mut file = self
            .file
            .lock()
            .map_err(|_| MapError::Internal("metadata file lock poisoned".into()))?;
        Self::check_length(&file, self.expected_len)?;
        let next_bytes = self
            .bytes_read
            .load(Ordering::Relaxed)
            .checked_add(u64::try_from(self.row_bytes).map_err(|_| {
                MapError::Invalid("metadata row byte count cannot fit in u64".into())
            })?)
            .ok_or_else(|| MapError::Budget("metadata byte counter overflow".into()))?;
        let next_rows = self
            .rows_read
            .load(Ordering::Relaxed)
            .checked_add(1)
            .ok_or_else(|| MapError::Budget("metadata row counter overflow".into()))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(self.row_bytes)
            .map_err(|_| MapError::Budget("metadata row allocation failed".into()))?;
        bytes.resize(self.row_bytes, 0);
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut bytes)?;
        Self::check_length(&file, self.expected_len)?;
        self.bytes_read.store(next_bytes, Ordering::Relaxed);
        self.rows_read.store(next_rows, Ordering::Relaxed);
        Ok(bytes)
    }

    /// Counts successful row reads, including repeated reads of the same row.
    /// Standard maps serialize without requiring a dependency in this module.
    pub(crate) fn stats(&self) -> BTreeMap<&'static str, u64> {
        // The read lock makes the two counters a coherent snapshot. A poisoned
        // lock still permits reporting the last successfully committed counts.
        let _guard = self.file.lock().unwrap_or_else(|error| error.into_inner());
        BTreeMap::from([
            ("bytes_read", self.bytes_read.load(Ordering::Relaxed)),
            ("rows_read", self.rows_read.load(Ordering::Relaxed)),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::PathBuf;

    static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);
    impl Fixture {
        fn new(bytes: &[u8]) -> Self {
            let path = std::env::temp_dir().join(format!(
                "fast-map-rowfile-{}-{}-{}.bin",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT_FILE.fetch_add(1, Ordering::Relaxed)
            ));
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .unwrap();
            file.write_all(bytes).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    #[test]
    fn preserves_little_endian_float_payloads_and_noncontiguous_order() {
        let words = [0x8000_0000u32, 0x7fc0_1234, 0x0000_0000, 0xff80_0000];
        let bytes: Vec<u8> = words.into_iter().flat_map(u32::to_le_bytes).collect();
        let fixture = Fixture::new(&bytes);
        let rows = RowFile::open(&fixture.0, 4, 4).unwrap();
        for row in [3, 0, 2, 1, 0] {
            assert_eq!(rows.read(row).unwrap(), words[row].to_le_bytes());
        }
        assert_eq!(
            rows.stats(),
            BTreeMap::from([("bytes_read", 20), ("rows_read", 5)])
        );
    }

    #[test]
    fn concurrent_reads_do_not_share_an_unlocked_cursor() {
        let bytes: Vec<u8> = (0..128u32).flat_map(u32::to_le_bytes).collect();
        let fixture = Fixture::new(&bytes);
        let rows = RowFile::open(&fixture.0, 128, 4).unwrap();
        std::thread::scope(|scope| {
            for worker in 0..8 {
                let rows = &rows;
                scope.spawn(move || {
                    for item in 0..128 {
                        let row = (item * 17 + worker * 13) % 128;
                        assert_eq!(rows.read(row).unwrap(), (row as u32).to_le_bytes());
                    }
                });
            }
        });
        assert_eq!(rows.stats()["rows_read"], 1024);
        assert_eq!(rows.stats()["bytes_read"], 4096);
    }

    #[test]
    fn rejects_shape_overflow_and_nonmatching_lengths() {
        let fixture = Fixture::new(&[1, 2, 3, 4]);
        assert!(matches!(
            RowFile::open(&fixture.0, usize::MAX, 2),
            Err(MapError::Invalid(_))
        ));
        assert!(matches!(
            RowFile::open(&fixture.0, 1, 3),
            Err(MapError::Invalid(_))
        ));
        assert!(matches!(
            RowFile::open(&fixture.0, 2, 3),
            Err(MapError::Invalid(_))
        ));
    }

    #[test]
    fn out_of_bounds_refuses_before_file_length_check_and_does_not_count() {
        let fixture = Fixture::new(&[1, 2, 3, 4]);
        let rows = RowFile::open(&fixture.0, 2, 2).unwrap();
        OpenOptions::new()
            .write(true)
            .open(&fixture.0)
            .unwrap()
            .set_len(0)
            .unwrap();
        let error = rows.read(usize::MAX).unwrap_err().to_string();
        assert!(error.contains("outside 2 rows"));
        assert_eq!(rows.stats()["rows_read"], 0);
        assert_eq!(rows.stats()["bytes_read"], 0);
    }

    #[test]
    fn detects_truncation_and_growth_without_counting_failed_reads() {
        let fixture = Fixture::new(&[1, 2, 3, 4]);
        let rows = RowFile::open(&fixture.0, 2, 2).unwrap();
        assert_eq!(rows.read(1).unwrap(), [3, 4]);
        let writer = OpenOptions::new().write(true).open(&fixture.0).unwrap();
        writer.set_len(3).unwrap();
        assert!(matches!(rows.read(0), Err(MapError::Invalid(_))));
        writer.set_len(5).unwrap();
        assert!(matches!(rows.read(0), Err(MapError::Invalid(_))));
        assert_eq!(rows.stats()["rows_read"], 1);
        assert_eq!(rows.stats()["bytes_read"], 2);
    }

    #[test]
    fn empty_shapes_are_checked_without_allocation() {
        let fixture = Fixture::new(&[]);
        let empty = RowFile::open(&fixture.0, 0, 4).unwrap();
        assert!(matches!(empty.read(0), Err(MapError::Invalid(_))));
        let zero_width = RowFile::open(&fixture.0, 2, 0).unwrap();
        assert!(zero_width.read(1).unwrap().is_empty());
        assert_eq!(zero_width.stats()["rows_read"], 1);
        assert_eq!(zero_width.stats()["bytes_read"], 0);
    }
}
