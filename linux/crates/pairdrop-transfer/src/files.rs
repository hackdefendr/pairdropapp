//! Reading files out in PairDrop's chunk/partition rhythm, and streaming incoming ones
//! straight to disk so a multi-gigabyte transfer never has to fit in memory.
//!
//! Both types are synchronous on purpose: the state machine hands them to
//! `spawn_blocking` rather than doing disk I/O on the async runtime.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use pairdrop_proto::{FileHeader, CHUNK_SIZE, MAX_PARTITION_SIZE};

// MARK: outgoing

pub struct FileChunker {
    file: File,
    pub size: u64,
    pub offset: u64,
    partition_bytes: usize,
}

impl FileChunker {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        Ok(Self { file, size, offset: 0, partition_bytes: 0 })
    }

    pub fn is_file_end(&self) -> bool {
        self.offset >= self.size
    }

    /// Note the `>=`: the threshold is crossed by a whole chunk rather than truncating
    /// one to land on it, which is what makes a partition 1,024,000 bytes rather than
    /// 1,000,000. The web client does the same, and matching it is the whole point.
    pub fn is_partition_end(&self) -> bool {
        self.partition_bytes >= MAX_PARTITION_SIZE
    }

    pub fn begin_partition(&mut self) {
        self.partition_bytes = 0;
    }

    /// The next chunk, or `None` at end of file.
    pub fn next_chunk(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        if self.is_file_end() {
            return Ok(None);
        }
        let mut buffer = vec![0u8; CHUNK_SIZE];
        let mut filled = 0;
        // `read` is allowed to return short; keep going until the chunk is full or the
        // file ends, or the receiver gets chunks of surprising sizes.
        while filled < buffer.len() {
            match self.file.read(&mut buffer[filled..])? {
                0 => break,
                n => filled += n,
            }
        }
        if filled == 0 {
            // The file shrank underneath us. Treat it as the end rather than looping.
            self.offset = self.size;
            return Ok(None);
        }
        buffer.truncate(filled);
        self.offset += filled as u64;
        self.partition_bytes += filled;
        Ok(Some(buffer))
    }

    /// Reads up to one whole partition, returning the chunks and whether the file ended.
    pub fn read_partition(&mut self) -> std::io::Result<(Vec<Vec<u8>>, bool)> {
        let mut chunks = Vec::with_capacity(MAX_PARTITION_SIZE / CHUNK_SIZE + 1);
        self.begin_partition();
        while !self.is_file_end() && !self.is_partition_end() {
            match self.next_chunk()? {
                Some(chunk) => chunks.push(chunk),
                None => break,
            }
        }
        Ok((chunks, self.is_file_end()))
    }
}

// MARK: incoming

pub struct FileReceiver {
    pub header: FileHeader,
    pub bytes_received: u64,
    temporary: PathBuf,
    file: File,
}

impl FileReceiver {
    pub fn create(header: FileHeader, staging: &Path) -> std::io::Result<Self> {
        fs::create_dir_all(staging)?;
        // Name the staging file after the process and a counter rather than the peer's
        // filename, which isn't trustworthy until it has been sanitised.
        let unique = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        );
        let temporary = staging.join(unique);
        let file = File::create(&temporary)?;
        Ok(Self { header, bytes_received: 0, temporary, file })
    }

    pub fn is_complete(&self) -> bool {
        self.bytes_received >= self.header.size.max(0) as u64
    }

    pub fn append(&mut self, data: &[u8]) -> std::io::Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        self.file.write_all(data)?;
        self.bytes_received += data.len() as u64;
        Ok(())
    }

    /// Closes the file and moves it into `directory`, avoiding collisions the way a
    /// file manager does — `name (2).ext`.
    pub fn finish(mut self, directory: &Path) -> std::io::Result<PathBuf> {
        self.file.flush()?;
        drop(self.file);

        fs::create_dir_all(directory)?;
        let destination = unique_path(directory, &self.header.name);

        // A rename across filesystems fails, which is normal when the staging directory
        // and the download folder are on different mounts.
        if fs::rename(&self.temporary, &destination).is_err() {
            fs::copy(&self.temporary, &destination)?;
            let _ = fs::remove_file(&self.temporary);
        }
        Ok(destination)
    }

    pub fn discard(self) {
        drop(self.file);
        let _ = fs::remove_file(&self.temporary);
    }
}

/// A peer chooses the filename, so strip anything that could escape the target
/// directory or hide the result.
pub fn sanitize(filename: &str) -> String {
    // Both separators: a Windows peer may send a backslash path, and `Path::file_name`
    // on Unix would keep it as part of the name.
    let last = filename
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(filename);

    let mut cleaned: String = last
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' => '_',
            '\0' => ' ',
            c => c,
        })
        .collect();

    cleaned = cleaned.trim().to_string();
    // A leading dot would hide the file; `..` would be worse.
    while cleaned.starts_with('.') {
        cleaned.remove(0);
        cleaned = cleaned.trim_start().to_string();
    }

    if cleaned.is_empty() {
        cleaned = "Received file".to_string();
    }
    cleaned.chars().take(200).collect()
}

/// `note.txt`, then `note (2).txt`, `note (3).txt`, …
pub fn unique_path(directory: &Path, filename: &str) -> PathBuf {
    let safe = sanitize(filename);
    let candidate = directory.join(&safe);
    if !candidate.exists() {
        return candidate;
    }

    let path = Path::new(&safe);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(&safe).to_string();
    let extension = path.extension().and_then(|s| s.to_str()).map(str::to_string);

    for counter in 2..10_000 {
        let name = match &extension {
            Some(ext) => format!("{stem} ({counter}).{ext}"),
            None => format!("{stem} ({counter})"),
        };
        let candidate = directory.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same cases the Swift implementation is pinned to, so a hostile filename can't
    /// behave differently on one platform.
    #[test]
    fn sanitizes_hostile_filenames() {
        assert_eq!(sanitize("../../etc/passwd"), "passwd");
        assert_eq!(sanitize("/etc/passwd"), "passwd");
        assert_eq!(sanitize(".bashrc"), "bashrc");
        assert_eq!(sanitize("a/b.txt"), "b.txt");
        assert_eq!(sanitize("   "), "Received file");
        assert_eq!(sanitize("ok name.txt"), "ok name.txt");
        assert_eq!(sanitize(".."), "Received file");
        assert_eq!(sanitize(""), "Received file");
    }

    /// A Windows peer sends backslashes, which `Path::file_name` would keep on Unix.
    #[test]
    fn strips_windows_paths_too() {
        assert_eq!(sanitize(r"C:\Windows\System32\evil.dll"), "evil.dll");
        assert_eq!(sanitize(r"..\..\secrets.txt"), "secrets.txt");
    }

    #[test]
    fn truncates_absurd_names() {
        let long = "a".repeat(500);
        assert_eq!(sanitize(&long).len(), 200);
    }

    #[test]
    fn avoids_overwriting_existing_files() {
        let dir = std::env::temp_dir().join(format!("pairdrop-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let _ = fs::remove_file(dir.join("note.txt"));
        let _ = fs::remove_file(dir.join("note (2).txt"));

        let first = unique_path(&dir, "note.txt");
        assert_eq!(first.file_name().unwrap(), "note.txt");
        fs::write(&first, b"x").unwrap();

        let second = unique_path(&dir, "note.txt");
        assert_eq!(second.file_name().unwrap(), "note (2).txt");

        fs::remove_dir_all(&dir).ok();
    }

    /// A partition is whole chunks that cross the threshold, not a truncated megabyte.
    #[test]
    fn partition_reads_sixteen_whole_chunks() {
        let dir = std::env::temp_dir().join(format!("pairdrop-chunk-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.bin");
        // Two partitions' worth plus a remainder.
        fs::write(&path, vec![7u8; 2_500_000]).unwrap();

        let mut chunker = FileChunker::open(&path).unwrap();
        let (first, done) = chunker.read_partition().unwrap();
        assert!(!done);
        assert_eq!(first.len(), 16, "a partition is 16 chunks");
        assert_eq!(first.iter().map(Vec::len).sum::<usize>(), 1_024_000);
        assert!(first.iter().all(|c| c.len() == CHUNK_SIZE));

        let (second, done) = chunker.read_partition().unwrap();
        assert!(!done);
        assert_eq!(second.iter().map(Vec::len).sum::<usize>(), 1_024_000);

        let (third, done) = chunker.read_partition().unwrap();
        assert!(done, "the third partition should reach the end");
        assert_eq!(third.iter().map(Vec::len).sum::<usize>(), 2_500_000 - 2_048_000);

        fs::remove_dir_all(&dir).ok();
    }
}
