use chrono::{DateTime, NaiveDate, Utc};
use std::{
    fs,
    io::{self, Write},
    path::Path,
};
use tracing_rolling_file::{RollingConditionBase, RollingFileAppenderBase};

pub(super) struct SizeLimitedWriter {
    inner: RollingFileAppenderBase,
    date: NaiveDate,
    bytes: u64,
    limit: u64,
    failed: bool,
}

impl SizeLimitedWriter {
    pub(super) fn new(directory: &Path, limit: u64, retained: usize) -> io::Result<Self> {
        if limit == 0 || retained < 2 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid log limits",
            ));
        }
        let path = directory.join("linkrd.log");
        if path.to_str().is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "log path must be Unicode",
            ));
        }
        let mut inner =
            RollingFileAppenderBase::new(&path, RollingConditionBase::new(), retained - 1)?;
        let metadata = fs::metadata(&path)?;
        let mut bytes = metadata.len();
        let date = Utc::now().date_naive();
        let modified = DateTime::<Utc>::from(metadata.modified()?).date_naive();
        if bytes > 0 && (bytes >= limit || modified != date) {
            inner.rollover()?;
            bytes = 0;
        }
        Ok(Self {
            inner,
            date,
            bytes,
            limit,
            failed: false,
        })
    }

    fn write_inner(&mut self, bytes: &[u8], date: NaiveDate) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        if date != self.date || bytes.len() as u64 > self.limit - self.bytes {
            if self.bytes > 0 {
                self.inner.rollover()?;
            }
            self.date = date;
            self.bytes = 0;
        }
        let count = bytes
            .len()
            .min(usize::try_from(self.limit - self.bytes).unwrap_or(usize::MAX));
        let written = self.inner.write(&bytes[..count])?;
        self.bytes += written as u64;
        Ok(written)
    }

    fn write_on(&mut self, bytes: &[u8], date: NaiveDate) -> io::Result<usize> {
        if self.failed {
            return Err(io::Error::other(
                "file logging disabled after I/O failure; restart required",
            ));
        }
        let result = self.write_inner(bytes, date);
        self.check_error(result)
    }

    fn check_error<T>(&mut self, result: io::Result<T>) -> io::Result<T> {
        result.inspect_err(|error| {
            if !self.failed {
                eprintln!("linkrd: file logging disabled after I/O failure: {error}; stderr logging continues; restart to retry");
            }
            self.failed = true;
        })
    }
}

impl Write for SizeLimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.write_on(bytes, Utc::now().date_naive())
    }

    fn flush(&mut self) -> io::Result<()> {
        let result = self.inner.flush();
        self.check_error(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{File, FileTimes};

    #[test]
    fn ten_mib_boundary_rotates_before_next_record_and_survives_restart() {
        let root = tempfile::tempdir().unwrap();
        let limit = super::super::MAX_LOG_BYTES;
        let active = root.path().join("linkrd.log");
        File::create(&active).unwrap().set_len(limit - 4).unwrap();
        let mut writer = SizeLimitedWriter::new(root.path(), limit, 7).unwrap();
        writer.write_all(b"abc\n").unwrap();
        writer.flush().unwrap();
        assert_eq!(fs::metadata(&active).unwrap().len(), limit);
        writer.write_all(b"next\n").unwrap();
        writer.flush().unwrap();
        assert_eq!(
            fs::metadata(root.path().join("linkrd.log.1"))
                .unwrap()
                .len(),
            limit
        );
        assert_eq!(fs::read(&active).unwrap(), b"next\n");
        drop(writer);
        let mut writer = SizeLimitedWriter::new(root.path(), limit, 7).unwrap();
        writer.write_all(b"restart\n").unwrap();
        writer.flush().unwrap();
        assert_eq!(fs::read(active).unwrap(), b"next\nrestart\n");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
    }

    #[test]
    fn oversized_writes_are_bounded_and_daily_rotation_keeps_recent_files() {
        let root = tempfile::tempdir().unwrap();
        let mut writer = SizeLimitedWriter::new(root.path(), 8, 7).unwrap();
        writer.write_all(b"0123456789abcdefghij").unwrap();
        writer.flush().unwrap();
        let reconstructed: Vec<u8> = ["linkrd.log.2", "linkrd.log.1", "linkrd.log"]
            .iter()
            .flat_map(|name| fs::read(root.path().join(name)).unwrap())
            .collect();
        assert_eq!(reconstructed, b"0123456789abcdefghij");
        let tomorrow = writer.date.succ_opt().unwrap();
        writer.write_on(b"new day\n", tomorrow).unwrap();
        writer.flush().unwrap();
        assert_eq!(writer.date, tomorrow);
        assert_eq!(
            fs::read(root.path().join("linkrd.log")).unwrap(),
            b"new day\n"
        );
        fs::write(root.path().join("unrelated.log"), b"keep").unwrap();
        fs::write(root.path().join("linkrd.2026-01-01.log"), b"legacy").unwrap();
        for _ in 0..10 {
            writer.write_on(b"full log", tomorrow).unwrap();
        }
        writer.flush().unwrap();
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 9);
        for index in 0..7 {
            let name = if index == 0 {
                "linkrd.log".into()
            } else {
                format!("linkrd.log.{index}")
            };
            assert!(fs::metadata(root.path().join(name)).unwrap().len() <= 8);
        }
        assert!(!root.path().join("linkrd.log.7").exists());
        assert_eq!(
            fs::read(root.path().join("unrelated.log")).unwrap(),
            b"keep"
        );
        assert_eq!(
            fs::read(root.path().join("linkrd.2026-01-01.log")).unwrap(),
            b"legacy"
        );
    }

    #[test]
    fn record_that_would_exceed_limit_rotates_without_splitting() {
        let root = tempfile::tempdir().unwrap();
        let mut writer = SizeLimitedWriter::new(root.path(), 8, 7).unwrap();
        writer.write_all(b"first\n").unwrap();
        writer.write_all(b"second\n").unwrap();
        writer.flush().unwrap();
        assert_eq!(
            fs::read(root.path().join("linkrd.log.1")).unwrap(),
            b"first\n"
        );
        assert_eq!(
            fs::read(root.path().join("linkrd.log")).unwrap(),
            b"second\n"
        );
    }

    #[test]
    fn existing_full_or_oversized_log_is_archived_before_writing() {
        for contents in [&b"12345678"[..], &b"old oversized log"[..]] {
            let root = tempfile::tempdir().unwrap();
            fs::write(root.path().join("linkrd.log"), contents).unwrap();
            let mut writer = SizeLimitedWriter::new(root.path(), 8, 7).unwrap();
            writer.write_all(b"new\n").unwrap();
            writer.flush().unwrap();
            assert_eq!(
                fs::read(root.path().join("linkrd.log.1")).unwrap(),
                contents
            );
            assert_eq!(fs::read(root.path().join("linkrd.log")).unwrap(), b"new\n");
        }
    }

    #[test]
    fn restart_after_utc_day_change_rotates_existing_log() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("linkrd.log");
        fs::write(&path, b"yesterday\n").unwrap();
        let yesterday = Utc::now() - chrono::Duration::days(1);
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(yesterday.into()))
            .unwrap();
        let mut writer = SizeLimitedWriter::new(root.path(), 128, 7).unwrap();
        writer.write_all(b"today\n").unwrap();
        writer.flush().unwrap();
        assert_eq!(
            fs::read(root.path().join("linkrd.log.1")).unwrap(),
            b"yesterday\n"
        );
        assert_eq!(fs::read(path).unwrap(), b"today\n");
    }

    #[test]
    fn rotation_failure_stops_writes_instead_of_exceeding_limit() {
        let root = tempfile::tempdir().unwrap();
        let mut writer = SizeLimitedWriter::new(root.path(), 8, 7).unwrap();
        writer.write_all(b"12345678").unwrap();
        writer.flush().unwrap();
        fs::create_dir(root.path().join("linkrd.log.6")).unwrap();
        fs::write(root.path().join("linkrd.log.6/keep"), b"keep").unwrap();
        fs::write(root.path().join("linkrd.log.5"), b"archive").unwrap();
        assert!(writer.write_all(b"must not write").is_err());
        assert!(writer.write_all(b"retry must not write").is_err());
        for entry in fs::read_dir(root.path()).unwrap() {
            let metadata = entry.unwrap().metadata().unwrap();
            if metadata.is_file() {
                assert!(metadata.len() <= 8);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_directory_is_an_error_not_a_library_panic() {
        use std::os::unix::ffi::OsStringExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(std::ffi::OsString::from_vec(vec![0xff]));
        assert!(SizeLimitedWriter::new(&path, 8, 7).is_err());
    }
}
