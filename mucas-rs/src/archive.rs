//! MCAR — μCAS multi-file archive format.
//!
//! Binary layout (all multi-byte integers are little-endian):
//!
//!   Header (10 bytes):
//!     [0]  magic:        [u8; 4] = b"MCAR"
//!     [4]  version:      u8 = 0x01
//!     [5]  flags:        u8 — 0x00 = baseline; 0x01 = ARCHIVE_FLAG_HAS_CONSENSUS
//!     [6]  entry_count:  u32
//!
//!   Consensus section (present when flags & 0x01 == 0x01):
//!     section_len:       u32  — byte count of the section body (not including section_len)
//!     count:             u32  — number of patterns
//!     for each pattern (sorted by hash key):
//!       hash_len:        u8
//!       hash:            [u8; hash_len]
//!       pat_len:         u32
//!       pat:             [u8; pat_len]
//!
//!   Entry (repeated entry_count times):
//!     path_len:          u16       — byte length of UTF-8 relative path
//!     path:              [u8]      — '/' separators
//!     method:            u8        — 0=Store, 1=Zlib, 2=MuCAS
//!     original_size:     u64
//!     compressed_size:   u32
//!     data:              [u8; compressed_size]
//!
//! v0.9 architecture:
//!   - ArchiveWriter<W: Write>: two-pass streaming write; entry_count must be
//!     known before the first write, so callers collect paths first, then stream.
//!     AlreadyCompressed files are stream-copied with a 64 KB buffer (constant
//!     memory regardless of file size). Other files are loaded one at a time and
//!     released immediately after writing.
//!   - ArchiveReader<R: Read>: streaming read; yields one entry at a time and
//!     releases its memory before reading the next.
//!   - compress_archive / decompress_archive: backwards-compat wrappers for tests.
//!
//! v0.12 change: consensus dictionary is stored ONCE in the archive header (not per-file).
//!   REF tokens in individual MucasFile programs reference archive-level patterns.
//!   MucasFile objects are created with `new()` (no embedded consensus); the global
//!   consensus is injected at decompress time from the archive header.
//!   Backward compat: v0.11 per-file consensus is still read correctly (file.consensus
//!   is non-empty → used in preference over the empty global_consensus from an old reader).

use std::io::{self, BufWriter, Read, Write};
use std::path::Path;

use crate::format::{compress_zlib, decompress_zlib, MucasFile};
use crate::pipeline::Pipeline;
use crate::sched::{classify_with_data, is_already_compressed, ClassMetrics, DataClass};
use crate::lz::LzEncoder;
use crate::synth::{shannon_entropy_byte, apply_ref_pass};
use crate::{VmState, Consensus};

/// Files smaller than this use Zlib-only (skip μCAS synthesis).
/// Synthesis overhead exceeds benefit for short inputs; Zlib is competitive.
const SMALL_FILE_THRESHOLD: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const MAGIC:                    [u8; 4] = [0x4D, 0x43, 0x41, 0x52]; // "MCAR"
pub const VERSION:                  u8      = 0x01;
pub const ARCHIVE_FLAG_HAS_CONSENSUS: u8    = 0x01;
pub const DEFAULT_MAX_MEMORY:       usize   = 256 * 1024 * 1024;         // 256 MiB
const STREAM_BUF:                   usize   = 64 * 1024;                  // 64 KiB copy buffer

// ---------------------------------------------------------------------------
// Method
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Method {
    Store = 0,
    Zlib  = 1,
    MuCAS = 2,
}

impl TryFrom<u8> for Method {
    type Error = ArchiveError;
    fn try_from(b: u8) -> Result<Self, ArchiveError> {
        match b {
            0 => Ok(Method::Store),
            1 => Ok(Method::Zlib),
            2 => Ok(Method::MuCAS),
            _ => Err(ArchiveError::UnknownMethod(b)),
        }
    }
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
pub enum ArchiveError {
    TooShort,
    BadMagic,
    UnsupportedVersion(u8),
    UnknownMethod(u8),
    InvalidPath(usize),
    EntryDecompressFailed { index: usize },
    MuCASDecompressFailed { index: usize },
    CountMismatch { expected: u32, actual: u32 },
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArchiveError::TooShort                    => write!(f, "archive too short"),
            ArchiveError::BadMagic                    => write!(f, "not a .mcar archive (bad magic)"),
            ArchiveError::UnsupportedVersion(v)       => write!(f, "unsupported .mcar version 0x{v:02X}"),
            ArchiveError::UnknownMethod(m)            => write!(f, "unknown compression method {m}"),
            ArchiveError::InvalidPath(i)              => write!(f, "entry {i}: path is not valid UTF-8"),
            ArchiveError::EntryDecompressFailed { index } =>
                write!(f, "entry {index}: zlib decompression failed"),
            ArchiveError::MuCASDecompressFailed { index } =>
                write!(f, "entry {index}: μCAS decode/execute failed"),
            ArchiveError::CountMismatch { expected, actual } =>
                write!(f, "entry count mismatch: header said {expected}, wrote {actual}"),
        }
    }
}

impl std::error::Error for ArchiveError {}

// ---------------------------------------------------------------------------
// Write-time statistics
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct WriteStats {
    pub store_count:      usize,
    pub zlib_count:       usize,
    pub mucas_count:      usize,
    pub total_original:   u64,
    pub total_compressed: u64,
}

impl WriteStats {
    pub fn compression_ratio(&self) -> f64 {
        if self.total_original == 0 { 1.0 }
        else { self.total_compressed as f64 / self.total_original as f64 }
    }
}

// ---------------------------------------------------------------------------
// ArchiveWriter  (streaming, W: Write only — no Seek needed)
// ---------------------------------------------------------------------------
//
// Caller must provide `entry_count` upfront (first scan file paths, then write).
// This lets us write the correct count into the header immediately, with no
// seek-back required.

pub struct ArchiveWriter<W: Write> {
    inner:           BufWriter<W>,
    expected_count:  u32,
    actual_count:    u32,
    max_memory:      usize,
    pub stats:       WriteStats,
    pub last_method: Option<Method>,
    /// When Some, REF synthesis is applied to compressible entries.
    consensus:       Option<Consensus>,
}

impl<W: Write> ArchiveWriter<W> {
    /// Create a writer.  `entry_count` is the total number of files to be added;
    /// it must be exact — `finish()` will error if the actual count differs.
    pub fn new(w: W, entry_count: u32) -> io::Result<Self> {
        Self::with_options(w, entry_count, DEFAULT_MAX_MEMORY)
    }

    pub fn with_options(w: W, entry_count: u32, max_memory: usize) -> io::Result<Self> {
        Self::with_consensus_options(w, entry_count, max_memory, None)
    }

    /// Like `with_options` but attaches a consensus dictionary for archive-level REF synthesis.
    /// The consensus is serialized once into the archive header; individual MucasFile entries
    /// do NOT embed their own consensus (REF tokens reference the archive-level patterns).
    /// Pass `None` for standard (no REF) behaviour.
    pub fn with_consensus_options(
        w: W, entry_count: u32, max_memory: usize, consensus: Option<Consensus>,
    ) -> io::Result<Self> {
        let mut inner = BufWriter::new(w);

        // Determine whether we write a consensus section.
        let has_consensus = matches!(&consensus, Some(c) if !c.is_empty());
        let flags: u8 = if has_consensus { ARCHIVE_FLAG_HAS_CONSENSUS } else { 0x00 };

        inner.write_all(&MAGIC)?;
        inner.write_all(&[VERSION, flags])?;
        inner.write_all(&entry_count.to_le_bytes())?;

        // Write archive-level consensus section immediately after entry_count.
        if has_consensus {
            let section = serialize_consensus(consensus.as_ref().unwrap());
            inner.write_all(&(section.len() as u32).to_le_bytes())?;
            inner.write_all(&section)?;
        }

        Ok(ArchiveWriter {
            inner,
            expected_count: entry_count,
            actual_count:   0,
            max_memory,
            stats:          WriteStats::default(),
            last_method:    None,
            consensus,
        })
    }

    /// Add a file from in-memory bytes.
    /// Used by `compress_archive()` wrapper and unit tests.
    pub fn add_file_bytes(&mut self, rel_path: &str, data: &[u8]) -> io::Result<()> {
        let (method, comp) = compress_entry_data(data, self.max_memory, self.consensus.as_ref());
        self.write_entry(rel_path, method, data.len() as u64, &comp)
    }

    /// Add a file from the filesystem.
    ///
    /// AlreadyCompressed files (JPEG, MP4, ZIP, …) are stream-copied with a
    /// 64 KiB buffer — memory usage is constant regardless of file size.
    /// Files larger than `max_memory` are also Stored without loading.
    /// Other files are loaded once, compressed, then released.
    pub fn add_file_path(&mut self, rel_path: &str, abs_path: &Path) -> io::Result<()> {
        let meta          = std::fs::metadata(abs_path)?;
        let original_size = meta.len();

        if original_size > u32::MAX as u64 {
            // Individual files > 4 GB are stored; current compressed_size field is u32.
            // (A future format version can extend this to u64.)
            return self.store_streaming(rel_path, original_size, abs_path);
        }

        // Peek at the first 12 bytes to check for compressed-format magic.
        let already_compressed = peek_is_already_compressed(abs_path)
            .unwrap_or(false); // if unreadable, fall through to load attempt

        if already_compressed || original_size > self.max_memory as u64 {
            self.store_streaming(rel_path, original_size, abs_path)
        } else {
            let data = std::fs::read(abs_path)?;
            self.add_file_bytes(rel_path, &data)
        }
    }

    /// Flush and consume the writer.  Returns the underlying `W`.
    /// Errors if the number of entries written ≠ `entry_count` given at construction.
    pub fn finish(self) -> io::Result<W> {
        let actual   = self.actual_count;
        let expected = self.expected_count;
        let inner = self.inner
            .into_inner()
            .map_err(|e| e.into_error())?;
        if actual != expected {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("entry count mismatch: header said {expected}, wrote {actual}"),
            ));
        }
        Ok(inner)
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    fn store_streaming(&mut self, rel_path: &str, original_size: u64, abs_path: &Path) -> io::Result<()> {
        self.write_entry_header(rel_path, Method::Store, original_size, original_size as u32)?;
        stream_copy_to(abs_path, &mut self.inner)?;
        self.record(Method::Store, original_size, original_size);
        Ok(())
    }

    fn write_entry(&mut self, path: &str, method: Method, orig: u64, comp: &[u8]) -> io::Result<()> {
        self.write_entry_header(path, method, orig, comp.len() as u32)?;
        self.inner.write_all(comp)?;
        self.record(method, orig, comp.len() as u64);
        Ok(())
    }

    fn write_entry_header(
        &mut self, path: &str, method: Method, orig: u64, comp_size: u32,
    ) -> io::Result<()> {
        let pb = path.as_bytes();
        self.inner.write_all(&(pb.len() as u16).to_le_bytes())?;
        self.inner.write_all(pb)?;
        self.inner.write_all(&[method as u8])?;
        self.inner.write_all(&orig.to_le_bytes())?;
        self.inner.write_all(&comp_size.to_le_bytes())?;
        Ok(())
    }

    fn record(&mut self, method: Method, orig: u64, comp: u64) {
        self.actual_count += 1;
        self.last_method   = Some(method);
        self.stats.total_original   += orig;
        self.stats.total_compressed += comp;
        match method {
            Method::Store => self.stats.store_count += 1,
            Method::Zlib  => self.stats.zlib_count  += 1,
            Method::MuCAS => self.stats.mucas_count += 1,
        }
    }
}

// ---------------------------------------------------------------------------
// ArchiveReader  (streaming, R: Read only)
// ---------------------------------------------------------------------------
//
// Each call to `next_entry()` reads exactly one entry from the archive and
// decompresses it.  Only one entry's data is in memory at a time.

pub struct ArchiveReader<R: Read> {
    inner:            R,
    remaining:        u32,
    total:            u32,
    index:            usize,
    /// Archive-level consensus loaded from header (v0.12+). Empty for older archives.
    global_consensus: Consensus,
}

pub struct DecodedEntry {
    pub path:    String,
    pub method:  Method,
    pub data:    Vec<u8>,
}

impl<R: Read> ArchiveReader<R> {
    pub fn new(mut r: R) -> Result<Self, ArchiveError> {
        let mut hdr = [0u8; 10];
        r.read_exact(&mut hdr).map_err(|_| ArchiveError::TooShort)?;
        if hdr[0..4] != MAGIC  { return Err(ArchiveError::BadMagic); }
        if hdr[4] != VERSION   { return Err(ArchiveError::UnsupportedVersion(hdr[4])); }
        let flags = hdr[5];
        let total = u32::from_le_bytes(hdr[6..10].try_into().unwrap());

        // Parse archive-level consensus section if present.
        let global_consensus = if flags & ARCHIVE_FLAG_HAS_CONSENSUS != 0 {
            let mut sl = [0u8; 4];
            r.read_exact(&mut sl).map_err(|_| ArchiveError::TooShort)?;
            let section_len = u32::from_le_bytes(sl) as usize;
            let mut buf = vec![0u8; section_len];
            r.read_exact(&mut buf).map_err(|_| ArchiveError::TooShort)?;
            parse_consensus_section(&buf).map_err(|_| ArchiveError::TooShort)?
        } else {
            Consensus::new()
        };

        Ok(ArchiveReader { inner: r, remaining: total, total, index: 0, global_consensus })
    }

    pub fn entry_count(&self) -> u32 { self.total }

    /// Read and decompress the next entry.  Returns `Ok(None)` when exhausted.
    pub fn next_entry(&mut self) -> Result<Option<DecodedEntry>, ArchiveError> {
        if self.remaining == 0 { return Ok(None); }

        let index = self.index;

        // Read path.
        let mut pl = [0u8; 2];
        self.inner.read_exact(&mut pl).map_err(|_| ArchiveError::TooShort)?;
        let path_len = u16::from_le_bytes(pl) as usize;
        let mut path_buf = vec![0u8; path_len];
        self.inner.read_exact(&mut path_buf).map_err(|_| ArchiveError::TooShort)?;
        let path = String::from_utf8(path_buf).map_err(|_| ArchiveError::InvalidPath(index))?;

        // Read fixed-size fields: method(1) + original_size(8) + compressed_size(4).
        let mut fixed = [0u8; 13];
        self.inner.read_exact(&mut fixed).map_err(|_| ArchiveError::TooShort)?;
        let method          = Method::try_from(fixed[0])?;
        let _original_size  = u64::from_le_bytes(fixed[1..9].try_into().unwrap());
        let compressed_size = u32::from_le_bytes(fixed[9..13].try_into().unwrap()) as usize;

        // Read compressed data.
        let mut comp = vec![0u8; compressed_size];
        self.inner.read_exact(&mut comp).map_err(|_| ArchiveError::TooShort)?;

        // Decompress — pass archive-level consensus for REF lookup.
        let data = decompress_entry(method, &comp, index, &self.global_consensus)?;

        self.remaining -= 1;
        self.index += 1;
        Ok(Some(DecodedEntry { path, method, data }))
    }
}

// ---------------------------------------------------------------------------
// EntryInfo — metadata-only listing (no decompression)
// ---------------------------------------------------------------------------

pub struct EntryInfo {
    pub path:            String,
    pub method:          Method,
    pub original_size:   u64,
    pub compressed_size: u64,
}

impl EntryInfo {
    pub fn ratio(&self) -> f64 {
        if self.original_size == 0 { 1.0 }
        else { self.compressed_size as f64 / self.original_size as f64 }
    }
}

/// Scan an MCAR stream and return per-entry metadata without decompressing data.
pub fn list_archive<R: Read>(mut r: R) -> Result<Vec<EntryInfo>, ArchiveError> {
    let mut hdr = [0u8; 10];
    r.read_exact(&mut hdr).map_err(|_| ArchiveError::TooShort)?;
    if hdr[0..4] != MAGIC  { return Err(ArchiveError::BadMagic); }
    if hdr[4] != VERSION   { return Err(ArchiveError::UnsupportedVersion(hdr[4])); }
    let flags = hdr[5];
    let count = u32::from_le_bytes(hdr[6..10].try_into().unwrap()) as usize;

    // Skip archive-level consensus section if present.
    if flags & ARCHIVE_FLAG_HAS_CONSENSUS != 0 {
        let mut sl = [0u8; 4];
        r.read_exact(&mut sl).map_err(|_| ArchiveError::TooShort)?;
        let section_len = u32::from_le_bytes(sl) as u64;
        io::copy(&mut r.by_ref().take(section_len), &mut io::sink())
            .map_err(|_| ArchiveError::TooShort)?;
    }

    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        let mut pl = [0u8; 2];
        r.read_exact(&mut pl).map_err(|_| ArchiveError::TooShort)?;
        let path_len = u16::from_le_bytes(pl) as usize;
        let mut path_buf = vec![0u8; path_len];
        r.read_exact(&mut path_buf).map_err(|_| ArchiveError::TooShort)?;
        let path = String::from_utf8(path_buf).map_err(|_| ArchiveError::InvalidPath(i))?;

        let mut fixed = [0u8; 13];
        r.read_exact(&mut fixed).map_err(|_| ArchiveError::TooShort)?;
        let method          = Method::try_from(fixed[0])?;
        let original_size   = u64::from_le_bytes(fixed[1..9].try_into().unwrap());
        let compressed_size = u32::from_le_bytes(fixed[9..13].try_into().unwrap()) as u64;

        // Skip over compressed data without reading it all into memory.
        io::copy(&mut r.by_ref().take(compressed_size), &mut io::sink())
            .map_err(|_| ArchiveError::TooShort)?;

        entries.push(EntryInfo { path, method, original_size, compressed_size });
    }
    Ok(entries)
}

// ---------------------------------------------------------------------------
// Backwards-compat wrappers  (used by unit tests and v0.8 callers)
// ---------------------------------------------------------------------------

/// Compress all entries into MCAR bytes (in-memory, for tests).
pub fn compress_archive(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    use std::io::Cursor;
    let count  = entries.len() as u32;
    let cursor = Cursor::new(Vec::<u8>::new());
    let mut w  = ArchiveWriter::new(cursor, count).expect("writer init");
    for (path, data) in entries {
        w.add_file_bytes(path, data).expect("add entry");
    }
    w.finish().expect("finish").into_inner()
}

/// Decompress all entries from MCAR bytes (in-memory, for tests).
pub fn decompress_archive(data: &[u8]) -> Result<Vec<(String, Vec<u8>)>, ArchiveError> {
    use std::io::Cursor;
    let mut reader = ArchiveReader::new(Cursor::new(data))?;
    let mut result = Vec::with_capacity(reader.entry_count() as usize);
    while let Some(entry) = reader.next_entry()? {
        result.push((entry.path, entry.data));
    }
    Ok(result)
}

/// Compute summary stats from raw MCAR bytes without decompressing file data.
pub fn archive_summary(data: &[u8]) -> Result<ArchiveSummary, ArchiveError> {
    if data.len() < 10 { return Err(ArchiveError::TooShort); }
    if data[0..4] != MAGIC { return Err(ArchiveError::BadMagic); }
    if data[4] != VERSION  { return Err(ArchiveError::UnsupportedVersion(data[4])); }

    let flags       = data[5];
    let entry_count = u32::from_le_bytes(data[6..10].try_into().unwrap()) as usize;
    let mut pos     = 10usize;

    // Skip archive-level consensus section if present.
    if flags & ARCHIVE_FLAG_HAS_CONSENSUS != 0 {
        if pos + 4 > data.len() { return Err(ArchiveError::TooShort); }
        let section_len = u32::from_le_bytes(data[pos..pos+4].try_into().unwrap()) as usize;
        pos += 4 + section_len;
        if pos > data.len() { return Err(ArchiveError::TooShort); }
    }

    let mut s = ArchiveSummary {
        file_count: entry_count,
        total_original: 0, total_compressed: 0,
        store_count: 0, zlib_count: 0, mucas_count: 0,
    };

    for _ in 0..entry_count {
        if pos + 2 > data.len() { return Err(ArchiveError::TooShort); }
        let path_len = u16::from_le_bytes(data[pos..pos+2].try_into().unwrap()) as usize;
        pos += 2 + path_len;
        if pos + 13 > data.len() { return Err(ArchiveError::TooShort); }
        let method          = Method::try_from(data[pos])?;
        let original_size   = u64::from_le_bytes(data[pos+1..pos+9].try_into().unwrap());
        let compressed_size = u32::from_le_bytes(data[pos+9..pos+13].try_into().unwrap()) as usize;
        pos += 13 + compressed_size;
        s.total_original   += original_size;
        s.total_compressed += compressed_size as u64;
        match method {
            Method::Store => s.store_count += 1,
            Method::Zlib  => s.zlib_count  += 1,
            Method::MuCAS => s.mucas_count += 1,
        }
    }
    Ok(s)
}

pub struct ArchiveSummary {
    pub file_count:       usize,
    pub total_original:   u64,
    pub total_compressed: u64,
    pub store_count:      usize,
    pub zlib_count:       usize,
    pub mucas_count:      usize,
}

impl ArchiveSummary {
    pub fn compression_ratio(&self) -> f64 {
        if self.total_original == 0 { 1.0 }
        else { self.total_compressed as f64 / self.total_original as f64 }
    }
}

// ---------------------------------------------------------------------------
// Per-entry compression  (in-memory)
// ---------------------------------------------------------------------------

/// Compress `data` using the best available method, or Store if nothing wins.
/// Respects `max_memory`: if `data.len() > max_memory`, force Store.
/// When `consensus` is Some, applies REF substitutions after μCAS synthesis.
/// REF patterns are stored in the archive-level consensus section, NOT embedded per-file.
fn compress_entry_data(data: &[u8], max_memory: usize, consensus: Option<&Consensus>) -> (Method, Vec<u8>) {
    if is_already_compressed(data) || data.len() > max_memory {
        return (Method::Store, data.to_vec());
    }

    // Fast entropy pre-check: skip expensive LZ scan for high-entropy (binary/encrypted) data.
    // O(n) byte-frequency pass — ~10x faster than the LZ sliding-window analysis.
    if shannon_entropy_byte(data) > 7.5 {
        return (Method::Store, data.to_vec());
    }

    // Small files: synthesis overhead exceeds benefit — Zlib-only is competitive and much faster.
    // Exception: when a consensus dictionary is available, try μCAS even on small files
    // so REF substitutions can fire (consensus lookup is free; synthesis overhead is small).
    if data.len() < SMALL_FILE_THRESHOLD && consensus.is_none() {
        let z = compress_zlib(data);
        return if z.len() < data.len() { (Method::Zlib, z) } else { (Method::Store, data.to_vec()) };
    }

    let analysis = LzEncoder::new().analyze(data);
    let metrics  = ClassMetrics::compute(data, &analysis);
    let class    = classify_with_data(&metrics, data);

    match class {
        DataClass::AlreadyCompressed | DataClass::Binary => {
            (Method::Store, data.to_vec())
        }
        DataClass::UnstructuredText => {
            let z = compress_zlib(data);
            if z.len() < data.len() { (Method::Zlib, z) } else { (Method::Store, data.to_vec()) }
        }
        _ => {
            // Try μCAS (with optional REF pass) and Zlib; take the smaller winner.
            let (mut prog, _) = Pipeline::new().compress(data);
            if let Some(c) = consensus {
                apply_ref_pass(&mut prog, c);
            }
            // v0.12: consensus lives in the archive header, not in each MucasFile.
            let (pb, subs) = prog.to_bytes();
            let mucas = MucasFile::new(pb, subs).encode();
            let zlib  = compress_zlib(data);
            let (best_method, best) =
                if mucas.len() <= zlib.len() { (Method::MuCAS, mucas) }
                else                          { (Method::Zlib,  zlib)  };
            if best.len() < data.len() {
                (best_method, best)
            } else {
                (Method::Store, data.to_vec())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Per-entry decompression
// ---------------------------------------------------------------------------

fn decompress_entry(
    method: Method, data: &[u8], index: usize, global_consensus: &Consensus,
) -> Result<Vec<u8>, ArchiveError> {
    match method {
        Method::Store => Ok(data.to_vec()),
        Method::Zlib  => decompress_zlib(data)
            .map_err(|_| ArchiveError::EntryDecompressFailed { index }),
        Method::MuCAS => {
            let file = MucasFile::decode(data)
                .map_err(|_| ArchiveError::MuCASDecompressFailed { index })?;
            // v0.11 archives embed per-file consensus; v0.12+ use archive-level.
            let consensus = if !file.consensus.is_empty() { &file.consensus } else { global_consensus };
            let mut vm = VmState::new();
            vm.exec(&file.program, &file.subs, consensus)
                .map_err(|_| ArchiveError::MuCASDecompressFailed { index })?;
            Ok(vm.output)
        }
    }
}

// ---------------------------------------------------------------------------
// Consensus serialization helpers
// ---------------------------------------------------------------------------

/// Serialize a consensus map to the archive-level section body (count + entries).
/// Sorted by hash key for deterministic output.
fn serialize_consensus(consensus: &Consensus) -> Vec<u8> {
    let mut sorted: Vec<(&Vec<u8>, &Vec<u8>)> = consensus.iter().collect();
    sorted.sort_by_key(|(h, _)| *h);

    let mut buf = Vec::new();
    buf.extend_from_slice(&(sorted.len() as u32).to_le_bytes());
    for (hash, pattern) in &sorted {
        buf.push(hash.len() as u8);
        buf.extend_from_slice(hash);
        buf.extend_from_slice(&(pattern.len() as u32).to_le_bytes());
        buf.extend_from_slice(pattern);
    }
    buf
}

/// Parse a consensus section body (count + entries) into a Consensus map.
fn parse_consensus_section(data: &[u8]) -> Result<Consensus, ()> {
    let mut pos = 0;
    if pos + 4 > data.len() { return Err(()); }
    let count = u32::from_le_bytes(data[pos..pos+4].try_into().unwrap()) as usize;
    pos += 4;

    let mut consensus = Consensus::new();
    for _ in 0..count {
        if pos >= data.len() { return Err(()); }
        let hash_len = data[pos] as usize;
        pos += 1;
        if pos + hash_len > data.len() { return Err(()); }
        let hash = data[pos..pos + hash_len].to_vec();
        pos += hash_len;

        if pos + 4 > data.len() { return Err(()); }
        let pat_len = u32::from_le_bytes(data[pos..pos+4].try_into().unwrap()) as usize;
        pos += 4;
        if pos + pat_len > data.len() { return Err(()); }
        let pattern = data[pos..pos + pat_len].to_vec();
        pos += pat_len;

        consensus.insert(hash, pattern);
    }
    Ok(consensus)
}

// ---------------------------------------------------------------------------
// Filesystem helpers
// ---------------------------------------------------------------------------

/// Read the first 12 bytes of a file and check for compressed-format magic.
pub fn peek_is_already_compressed(path: &Path) -> io::Result<bool> {
    use std::io::Read;
    let mut f   = std::fs::File::open(path)?;
    let mut buf = [0u8; 12];
    let n       = f.read(&mut buf)?;
    Ok(is_already_compressed(&buf[..n]))
}

/// Stream-copy a file to a `Write` in 64 KiB chunks.
fn stream_copy_to<W: Write>(src: &Path, dst: &mut W) -> io::Result<()> {
    let mut f   = std::fs::File::open(src)?;
    let mut buf = [0u8; STREAM_BUF];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 { break; }
        dst.write_all(&buf[..n])?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn fake_jpeg() -> Vec<u8> {
        let mut v = vec![0xFF_u8, 0xD8, 0xFF, 0xE0];
        v.extend_from_slice(&[0xAB; 200]);
        v
    }

    fn csv_data() -> Vec<u8> {
        b"id,name,status\n1,alice,active\n2,bob,active\n3,carol,active\n"
            .iter().cloned().cycle().take(3000).collect()
    }

    fn prose_data() -> Vec<u8> {
        b"The quick brown fox jumps over the lazy dog. Pack my box. "
            .iter().cloned().cycle().take(2000).collect()
    }

    fn round_trip(entries: &[(String, Vec<u8>)]) -> Vec<(String, Vec<u8>)> {
        let enc = compress_archive(entries);
        decompress_archive(&enc).unwrap()
    }

    // --- writer API ---

    #[test]
    fn archive_empty() {
        let enc = compress_archive(&[]);
        let dec = decompress_archive(&enc).unwrap();
        assert!(dec.is_empty());
    }

    #[test]
    fn archive_roundtrip_store() {
        let jpeg    = fake_jpeg();
        let entries = vec![("img/photo.jpg".to_string(), jpeg.clone())];
        let dec     = round_trip(&entries);
        assert_eq!(dec[0].0, "img/photo.jpg");
        assert_eq!(dec[0].1, jpeg);
    }

    #[test]
    fn archive_roundtrip_mucas() {
        let data    = csv_data();
        let entries = vec![("data.csv".to_string(), data.clone())];
        let dec     = round_trip(&entries);
        assert_eq!(dec[0].1, data, "μCAS round-trip failed");
    }

    #[test]
    fn archive_roundtrip_zlib() {
        let data    = prose_data();
        let entries = vec![("readme.txt".to_string(), data.clone())];
        let dec     = round_trip(&entries);
        assert_eq!(dec[0].1, data, "Zlib round-trip failed");
    }

    #[test]
    fn archive_roundtrip_mixed() {
        let entries: Vec<(String, Vec<u8>)> = vec![
            ("img/photo.jpg".to_string(), fake_jpeg()),
            ("data.csv".to_string(),      csv_data()),
            ("readme.txt".to_string(),    prose_data()),
        ];
        let orig: Vec<Vec<u8>> = entries.iter().map(|(_, d)| d.clone()).collect();
        let dec = round_trip(&entries);
        for (i, (path, bytes)) in dec.iter().enumerate() {
            assert_eq!(path,  &entries[i].0, "path mismatch at {i}");
            assert_eq!(bytes, &orig[i],       "data mismatch at {i}");
        }
    }

    #[test]
    fn archive_rejects_bad_magic() {
        let mut enc = compress_archive(&[]);
        enc[0] = 0xFF;
        assert_eq!(decompress_archive(&enc).unwrap_err(), ArchiveError::BadMagic);
    }

    #[test]
    fn archive_rejects_unsupported_version() {
        let mut enc = compress_archive(&[]);
        enc[4] = 0x02;
        assert_eq!(
            decompress_archive(&enc).unwrap_err(),
            ArchiveError::UnsupportedVersion(0x02)
        );
    }

    #[test]
    fn archive_summary_counts_methods() {
        let entries = vec![
            ("img/photo.jpg".to_string(), fake_jpeg()),
            ("data.csv".to_string(),      csv_data()),
        ];
        let enc = compress_archive(&entries);
        let s   = archive_summary(&enc).unwrap();
        assert_eq!(s.file_count,  2);
        assert_eq!(s.store_count, 1); // JPEG → Store
        assert!(s.total_compressed < s.total_original);
    }

    #[test]
    fn archive_preserves_entry_order() {
        let entries: Vec<(String, Vec<u8>)> =
            (0u8..10).map(|i| (format!("file{i}.csv"), csv_data())).collect();
        let dec = round_trip(&entries);
        for (i, (path, _)) in dec.iter().enumerate() {
            assert_eq!(path, &format!("file{i}.csv"));
        }
    }

    // --- ArchiveWriter / ArchiveReader streaming API ---

    #[test]
    fn writer_count_mismatch_errors() {
        let cursor = Cursor::new(Vec::<u8>::new());
        let mut w  = ArchiveWriter::new(cursor, 2).unwrap(); // claims 2 entries
        w.add_file_bytes("a.txt", b"hello").unwrap();         // write only 1
        let err = w.finish().unwrap_err();
        assert!(err.to_string().contains("mismatch"));
    }

    #[test]
    fn reader_yields_entries_in_order() {
        let entries: Vec<(String, Vec<u8>)> = vec![
            ("a.csv".to_string(), csv_data()),
            ("b.txt".to_string(), prose_data()),
        ];
        let enc    = compress_archive(&entries);
        let mut r  = ArchiveReader::new(Cursor::new(enc.as_slice())).unwrap();
        assert_eq!(r.entry_count(), 2);

        let e0 = r.next_entry().unwrap().unwrap();
        assert_eq!(e0.path, "a.csv");
        assert_eq!(e0.data, entries[0].1);

        let e1 = r.next_entry().unwrap().unwrap();
        assert_eq!(e1.path, "b.txt");
        assert_eq!(e1.data, entries[1].1);

        assert!(r.next_entry().unwrap().is_none());
    }

    #[test]
    fn store_entry_written_to_writer_stats() {
        let cursor = Cursor::new(Vec::<u8>::new());
        let mut w  = ArchiveWriter::new(cursor, 1).unwrap();
        w.add_file_bytes("img.jpg", &fake_jpeg()).unwrap();
        assert_eq!(w.last_method, Some(Method::Store));
        assert_eq!(w.stats.store_count, 1);
        w.finish().unwrap();
    }

    // --- Archive-level consensus (v0.12) ---

    #[test]
    fn archive_consensus_roundtrip() {
        // Build a 3-entry archive with a shared pattern across all entries.
        let shared = b"GLOBAL_CONSENSUS_PATTERN_ABCDE!"; // 31 bytes, high entropy
        assert_eq!(shared.len(), 31);

        // Each entry: shared pattern repeated 5x + unique suffix
        let make_entry = |suffix: &[u8]| -> Vec<u8> {
            let mut v = Vec::new();
            for _ in 0..5 { v.extend_from_slice(shared); }
            v.extend_from_slice(suffix);
            v
        };
        let d0 = make_entry(b"entry_zero_unique_data");
        let d1 = make_entry(b"entry_one_unique_data_here");
        let d2 = make_entry(b"entry_two_unique_data_here_now");

        // Build consensus from the shared pattern.
        let mut consensus = Consensus::new();
        consensus.insert(vec![0x00], shared.to_vec());

        let entries: Vec<(String, Vec<u8>)> = vec![
            ("e0.bin".to_string(), d0.clone()),
            ("e1.bin".to_string(), d1.clone()),
            ("e2.bin".to_string(), d2.clone()),
        ];

        // Write with archive-level consensus.
        let cursor = Cursor::new(Vec::<u8>::new());
        let mut w  = ArchiveWriter::with_consensus_options(
            cursor, 3, DEFAULT_MAX_MEMORY, Some(consensus),
        ).unwrap();
        for (path, data) in &entries {
            w.add_file_bytes(path, data).unwrap();
        }
        let enc = w.finish().unwrap().into_inner();

        // Check archive flags byte.
        assert_eq!(enc[5], ARCHIVE_FLAG_HAS_CONSENSUS, "flags must indicate consensus section");

        // Round-trip: decompress and verify.
        let dec = decompress_archive(&enc).unwrap();
        assert_eq!(dec[0].1, d0, "entry 0 mismatch");
        assert_eq!(dec[1].1, d1, "entry 1 mismatch");
        assert_eq!(dec[2].1, d2, "entry 2 mismatch");

        // list_archive and archive_summary must also parse correctly.
        let listing = list_archive(Cursor::new(enc.as_slice())).unwrap();
        assert_eq!(listing.len(), 3);
        let summary = archive_summary(&enc).unwrap();
        assert_eq!(summary.file_count, 3);
    }

    #[test]
    fn consensus_section_serialise_parse_roundtrip() {
        let mut c = Consensus::new();
        c.insert(vec![0x00], b"PATTERN_ALPHA_XXXX".to_vec());
        c.insert(vec![0x01], b"PATTERN_BETA_YYYY!".to_vec());
        let serialized = serialize_consensus(&c);
        let parsed = parse_consensus_section(&serialized).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed.get(&vec![0x00u8]).unwrap(), b"PATTERN_ALPHA_XXXX");
        assert_eq!(parsed.get(&vec![0x01u8]).unwrap(), b"PATTERN_BETA_YYYY!");
    }

    #[test]
    fn archive_without_consensus_unaffected() {
        // Non-consensus archives must still work exactly as before (flags=0x00).
        let entries = vec![("data.csv".to_string(), csv_data())];
        let enc = compress_archive(&entries);
        assert_eq!(enc[5], 0x00, "no-consensus archive must have flags=0x00");
        let dec = decompress_archive(&enc).unwrap();
        assert_eq!(dec[0].1, csv_data());
    }
}
