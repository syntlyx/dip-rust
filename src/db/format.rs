//! Content-based detection of database dump formats.
//!
//! Import used to trust the file name: anything ending in `.gz` was piped
//! through `gunzip -c`, anything else went straight to `psql`/`mysql`. So
//! `backup.tar.gz` decompressed to raw tar bytes and handed *those* to the
//! client, which failed with a wall of syntax errors partway through the
//! import — a half-applied dump, not a clear rejection. Detection now reads
//! the leading bytes and refuses what the importer cannot stream.

use std::fmt;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::Path;

use anyhow::{Context, Result};

/// Bytes to sniff. A tar header carries its magic at offset 257, so the
/// window has to reach past that.
const SNIFF_LEN: usize = 512;

/// A dump format `dip db import` can stream into a database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DumpFormat {
    /// Plain SQL text — fed to the client verbatim.
    Sql,
    /// gzipped plain SQL — decompressed inside the container.
    GzipSql,
}

impl fmt::Display for DumpFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Sql => "plain SQL",
            Self::GzipSql => "gzipped SQL",
        })
    }
}

/// Detect the format of `path`, or explain why it cannot be imported.
pub fn detect(path: &Path) -> Result<DumpFormat> {
    let head = read_head(&mut open(path)?)
        .with_context(|| format!("Cannot read import file {}", path.display()))?;

    match sniff(&head) {
        Kind::Sql => Ok(DumpFormat::Sql),
        // The gzip layer is supported, so what matters is what is inside it.
        Kind::Gzip => match gzip_inner(path)? {
            Kind::Sql => Ok(DumpFormat::GzipSql),
            inner => Err(unsupported(path, inner, true)),
        },
        other => Err(unsupported(path, other, false)),
    }
}

/// Whether a *dump* should be gzipped, decided by the output file name.
///
/// The counterpart of [`detect`] for the write path: the file does not exist
/// yet, so its name is the only signal — and the one the user chose.
pub fn gzip_output_requested(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("gz"))
}

// ─── sniffing ────────────────────────────────────────────────────────────────

/// Everything sniffing can recognize — supported or not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// No known container magic and the bytes look like text.
    Sql,
    Gzip,
    Tar,
    Zip,
    Zstd,
    Bzip2,
    Xz,
    Lz4,
    /// `pg_dump -Fc` / `-Ft` archive, which only `pg_restore` reads.
    PgDumpArchive,
    /// Unrecognized, and not text either.
    Binary,
    Empty,
}

fn sniff(head: &[u8]) -> Kind {
    if head.is_empty() {
        return Kind::Empty;
    }

    // Container magics, longest-prefix first so no shorter one shadows them.
    for (magic, kind) in [
        (&b"\xfd7zXZ\x00"[..], Kind::Xz),
        (&b"\x28\xb5\x2f\xfd"[..], Kind::Zstd),
        (&b"\x04\x22\x4d\x18"[..], Kind::Lz4),
        (&b"PK\x03\x04"[..], Kind::Zip),
        (&b"PGDMP"[..], Kind::PgDumpArchive),
        (&b"\x1f\x8b"[..], Kind::Gzip),
        (&b"BZh"[..], Kind::Bzip2),
    ] {
        if head.starts_with(magic) {
            return kind;
        }
    }

    // tar keeps `ustar` at offset 257 — GNU writes "ustar  \0", POSIX "ustar\000".
    if head.len() >= 262 && &head[257..262] == b"ustar" {
        return Kind::Tar;
    }

    if looks_like_text(head) {
        Kind::Sql
    } else {
        Kind::Binary
    }
}

/// Does the head read as SQL text?
///
/// Every dump client opens with an ASCII comment header, so a control byte this
/// early rules SQL out. The test is bytewise rather than a UTF-8 decode on
/// purpose: a latin1 dump is still a dump, and its high bytes should not be
/// mistaken for binary. Random bytes, meanwhile, hit a control byte within a
/// few dozen — which is what keeps garbage from reaching the database.
fn looks_like_text(head: &[u8]) -> bool {
    !head
        .iter()
        .any(|&b| (b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r')) || b == 0x7f)
}

/// Sniff the *decompressed* head of a gzip file.
fn gzip_inner(path: &Path) -> Result<Kind> {
    let mut decoder = flate2::read::GzDecoder::new(open(path)?);
    let head = read_head(&mut decoder).map_err(|e| {
        anyhow::anyhow!(
            "Cannot decompress {}: {e}\n  \
             The gzip header is valid but the stream does not inflate — the file \
             is probably truncated or corrupt.",
            path.display()
        )
    })?;
    Ok(sniff(&head))
}

// ─── io ──────────────────────────────────────────────────────────────────────

fn open(path: &Path) -> Result<File> {
    File::open(path).with_context(|| format!("Cannot open import file {}", path.display()))
}

/// Read up to [`SNIFF_LEN`] bytes, tolerating short reads and EOF.
fn read_head(source: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let mut head = vec![0u8; SNIFF_LEN];
    let mut filled = 0;
    while filled < head.len() {
        match source.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    head.truncate(filled);
    Ok(head)
}

// ─── errors ──────────────────────────────────────────────────────────────────

/// Explain a format the importer cannot stream, and how to get past it.
///
/// `gzipped` says the kind was found *inside* a gzip wrapper, which changes the
/// fix — the user has to unwrap two layers, not one.
fn unsupported(path: &Path, kind: Kind, gzipped: bool) -> anyhow::Error {
    let file = path.display();

    // Degenerate case: nothing to explain, and no format to name.
    if kind == Kind::Empty {
        return anyhow::anyhow!("{file} is empty — nothing to import.");
    }

    let (what, hint) = match kind {
        Kind::Tar => (
            "a tar archive",
            if gzipped {
                format!("Extract the dump first:  tar -xzOf {file} > dump.sql")
            } else {
                format!("Extract the dump first:  tar -xOf {file} > dump.sql")
            },
        ),
        Kind::Zip => (
            "a zip archive",
            format!("Extract the dump first:  unzip -p {file} > dump.sql"),
        ),
        Kind::Zstd => (
            "zstd-compressed",
            format!("Recompress as gzip:  zstd -dc {file} | gzip > dump.sql.gz"),
        ),
        Kind::Bzip2 => (
            "bzip2-compressed",
            format!("Recompress as gzip:  bzip2 -dc {file} | gzip > dump.sql.gz"),
        ),
        Kind::Xz => (
            "xz-compressed",
            format!("Recompress as gzip:  xz -dc {file} | gzip > dump.sql.gz"),
        ),
        Kind::Lz4 => (
            "lz4-compressed",
            format!("Recompress as gzip:  lz4 -dc {file} | gzip > dump.sql.gz"),
        ),
        Kind::PgDumpArchive => (
            "a pg_dump archive (custom or tar format)",
            format!(
                "Only pg_restore reads it. Convert it:  pg_restore -f dump.sql {file}\n  \
                 Or dump in plain text to begin with — `dip db dump` already does."
            ),
        ),
        Kind::Binary => (
            "not SQL text",
            "The leading bytes carry no readable header and match no format dip knows \
             — check that this is really a database dump."
                .to_string(),
        ),
        // Supported kinds, and Empty, never reach here.
        Kind::Sql | Kind::Gzip | Kind::Empty => ("unsupported", String::new()),
    };

    // Only the inner kind is named above, so the wrapper is called out here.
    let gz_note = if gzipped { ", gzipped" } else { "" };
    anyhow::anyhow!(
        "{file} is {what}{gz_note} — `dip db import` streams SQL straight into the \
         database client, so it cannot read this.\n  {hint}\n  \
         Supported: plain SQL and gzipped SQL (detected by content, not by file name)."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn sniffed(bytes: &[u8]) -> Kind {
        sniff(bytes)
    }

    /// Write `bytes` to a temp file and detect it.
    fn detect_bytes(bytes: &[u8]) -> Result<DumpFormat> {
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file().write_all(bytes).unwrap();
        detect(file.path())
    }

    fn gzip(payload: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(payload).unwrap();
        encoder.finish().unwrap()
    }

    /// A tar header is 512 bytes with `ustar` at 257; the rest can stay zeroed.
    fn tar_header() -> Vec<u8> {
        let mut header = vec![0u8; 512];
        header[..8].copy_from_slice(b"dump.sql");
        header[257..262].copy_from_slice(b"ustar");
        header
    }

    const PG_DUMP: &[u8] = b"--\n-- PostgreSQL database dump\n--\n\nCREATE TABLE t ();\n";

    // ── sniff() ───────────────────────────────────────────────────────────────

    #[test]
    fn sniffs_plain_sql() {
        assert_eq!(sniffed(PG_DUMP), Kind::Sql);
        assert_eq!(sniffed(b"-- MySQL dump 10.13\n"), Kind::Sql);
    }

    #[test]
    fn sniffs_container_magics() {
        assert_eq!(sniffed(&gzip(PG_DUMP)), Kind::Gzip);
        assert_eq!(sniffed(b"PK\x03\x04rest"), Kind::Zip);
        assert_eq!(sniffed(b"\x28\xb5\x2f\xfdrest"), Kind::Zstd);
        assert_eq!(sniffed(b"BZh9rest"), Kind::Bzip2);
        assert_eq!(sniffed(b"\xfd7zXZ\x00rest"), Kind::Xz);
        assert_eq!(sniffed(b"\x04\x22\x4d\x18rest"), Kind::Lz4);
        assert_eq!(sniffed(b"PGDMP\x01\x0e\x00"), Kind::PgDumpArchive);
        assert_eq!(sniffed(&tar_header()), Kind::Tar);
    }

    #[test]
    fn sniffs_empty_and_binary() {
        assert_eq!(sniffed(b""), Kind::Empty);
        assert_eq!(sniffed(b"\x89PNG\r\n\x1a\n\x00\x00"), Kind::Binary);
        assert_eq!(sniffed(b"SQLite format 3\x00"), Kind::Binary);
    }

    #[test]
    fn high_bytes_are_text_control_bytes_are_not() {
        // A latin1 dump is still a dump — high bytes must not read as binary.
        assert_eq!(sniffed(b"-- dump of caf\xe9\nSELECT 1;\n"), Kind::Sql);
        // Tab, newline and CR are the only control bytes SQL text carries.
        assert_eq!(sniffed(b"SELECT\t1,\r\n\t2;\n"), Kind::Sql);
        assert_eq!(sniffed(b"SELECT \x0b1;"), Kind::Binary);
        assert_eq!(sniffed(b"SELECT \x7f1;"), Kind::Binary);
    }

    #[test]
    fn short_text_file_is_sql_not_tar() {
        // Shorter than the tar magic offset — must not index out of bounds.
        assert_eq!(sniffed(b"SELECT 1;"), Kind::Sql);
    }

    // ── detect() ──────────────────────────────────────────────────────────────

    #[test]
    fn detects_plain_and_gzipped_sql() {
        assert_eq!(detect_bytes(PG_DUMP).unwrap(), DumpFormat::Sql);
        assert_eq!(detect_bytes(&gzip(PG_DUMP)).unwrap(), DumpFormat::GzipSql);
    }

    #[test]
    fn format_is_content_based_not_name_based() {
        // A gzipped dump named `.sql` still imports; the bytes decide.
        let file = tempfile::Builder::new().suffix(".sql").tempfile().unwrap();
        file.as_file().write_all(&gzip(PG_DUMP)).unwrap();
        assert_eq!(detect(file.path()).unwrap(), DumpFormat::GzipSql);
    }

    #[test]
    fn rejects_gzipped_tar_the_old_code_accepted() {
        // The regression this module exists for: `.tar.gz` used to pass as
        // gzipped SQL and pipe tar bytes into psql.
        let mut archive = tar_header();
        archive.extend_from_slice(&vec![0u8; 1024]);
        let err = detect_bytes(&gzip(&archive)).unwrap_err().to_string();
        assert!(err.contains("is a tar archive, gzipped"), "{err}");
        assert!(err.contains("tar -xzOf"), "{err}");
    }

    #[test]
    fn rejects_unsupported_formats_with_a_hint() {
        for (bytes, needle) in [
            (b"PK\x03\x04rest".to_vec(), "unzip -p"),
            (b"\x28\xb5\x2f\xfdrest".to_vec(), "zstd -dc"),
            (b"BZh9rest".to_vec(), "bzip2 -dc"),
            (b"\xfd7zXZ\x00rest".to_vec(), "xz -dc"),
            (b"\x04\x22\x4d\x18rest".to_vec(), "lz4 -dc"),
            (b"PGDMP\x01\x0e\x00".to_vec(), "pg_restore"),
            (tar_header(), "tar -xOf"),
        ] {
            let err = detect_bytes(&bytes).unwrap_err().to_string();
            assert!(err.contains(needle), "expected {needle:?} in: {err}");
        }
    }

    #[test]
    fn rejects_empty_file() {
        let err = detect_bytes(b"").unwrap_err().to_string();
        assert!(err.contains("empty"), "{err}");
    }

    #[test]
    fn rejects_corrupt_gzip() {
        // Valid gzip magic, garbage deflate stream.
        let mut corrupt = gzip(PG_DUMP);
        corrupt[12..20].fill(0xff);
        let err = detect_bytes(&corrupt).unwrap_err().to_string();
        assert!(err.contains("truncated or corrupt"), "{err}");
    }

    #[test]
    fn missing_file_reports_the_path() {
        let err = detect(Path::new("/nonexistent/dump.sql"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("/nonexistent/dump.sql"), "{err}");
    }

    // ── gzip_output_requested() ───────────────────────────────────────────────

    #[test]
    fn gzip_output_follows_the_extension() {
        assert!(gzip_output_requested(Path::new("backup.sql.gz")));
        assert!(gzip_output_requested(Path::new("backup.GZ")));
        assert!(!gzip_output_requested(Path::new("backup.sql")));
        assert!(!gzip_output_requested(Path::new("backup.gzip")));
    }
}
