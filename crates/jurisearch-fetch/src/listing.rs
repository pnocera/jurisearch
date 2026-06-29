//! Parser for DILA OPENDATA Apache `mod_autoindex` directory listings.
//!
//! The DILA "serveur d'échanges" exposes each dataset as a plain Apache
//! directory listing. Two HTML shapes are common across Apache versions:
//!
//! * the modern `<table>` layout (`IndexOptions HTMLTable`), one `<tr>` per
//!   file with name / last-modified / size columns; and
//! * the classic `<pre>`-formatted layout, one `<a href=...>` per line followed
//!   by a fixed-width date and size.
//!
//! Rather than parse either layout structurally, we extract every `href`
//! attribute (robust to both shapes and to icon `<img>` tags, `<hr>` separators,
//! and the `?C=N;O=D` column-sort links Apache emits), normalise it to a bare
//! file name, and best-effort attach the trailing last-modified / size columns
//! from the same row/line. Source filtering is then delegated to the ingest
//! name parser so cross-source and malformed names are rejected identically to
//! the on-disk planner.

use std::sync::LazyLock;

use regex::Regex;

use jurisearch_ingest::archive::{ArchiveSource, ParsedArchive};

/// A raw entry extracted from an Apache directory listing, before any
/// source-specific validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedEntry {
    /// Bare file name (no directory component, percent-decoded where trivial).
    pub name: String,
    /// Last-modified column as printed by Apache (e.g. `2025-07-14 00:10`), if
    /// present. Informational only.
    pub last_modified: Option<String>,
    /// Size column as printed by Apache (e.g. `42M`, `484K`, `1.1G`, or an exact
    /// byte count), if present. Human-readable sizes are *approximate* and are
    /// never used as an integrity equality check.
    pub size: Option<String>,
}

/// A listing entry whose name parsed as a valid archive for the requested
/// [`ArchiveSource`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteArchive {
    /// The validated archive (source, kind, timestamp, file name).
    pub parsed: ParsedArchive,
    /// Raw size column from the listing, if present (approximate).
    pub size: Option<String>,
    /// Last-modified column from the listing, if present.
    pub last_modified: Option<String>,
}

impl RemoteArchive {
    /// Convenience accessor for the archive file name.
    #[must_use]
    pub fn file_name(&self) -> &str {
        &self.parsed.file_name
    }
}

static HREF_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)<a\s+[^>]*?href\s*=\s*["']([^"']+)["'][^>]*>"#).expect("valid href regex")
});

// Matches an Apache date+size tail such as `2025-07-14 00:10   42M` or
// `2025-07-13 14:05  1.1G` or `... 484K` / `... 1234` / `... -`.
static TAIL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?P<date>\d{4}-\d{2}-\d{2}\s+\d{2}:\d{2})(?:\s+(?P<size>[\d.]+[KMGT]?|-|[\d.]+))?")
        .expect("valid tail regex")
});

/// Parse all file entries out of an Apache directory-listing HTML document.
///
/// Directory links (`href` ending in `/`), parent/sort links, absolute URLs,
/// and anchors with no usable file name are dropped. The result preserves
/// listing order and is *not* filtered to any source.
#[must_use]
pub fn parse_apache_index(html: &str) -> Vec<ListedEntry> {
    let mut entries = Vec::new();

    for capture in HREF_RE.captures_iter(html) {
        let href = &capture[1];
        let Some(name) = normalise_href(href) else {
            continue;
        };

        // Find the slice of HTML following this anchor to scrape the date/size
        // columns from the same row or `<pre>` line.
        let match_end = capture.get(0).map(|m| m.end()).unwrap_or(0);
        let tail = scrape_tail(&html[match_end..]);

        entries.push(ListedEntry {
            name,
            last_modified: tail.as_ref().map(|t| t.0.clone()),
            size: tail.and_then(|t| t.1),
        });
    }

    entries
}

/// Parse an Apache listing and keep only entries that are valid archives for
/// `source`. Cross-source names, sort links, and malformed names are rejected
/// by [`ParsedArchive::parse_file_name`] exactly as the on-disk planner rejects
/// them. The result is ordered by archive timestamp then file name.
#[must_use]
pub fn parse_source_listing(source: ArchiveSource, html: &str) -> Vec<RemoteArchive> {
    let mut archives: Vec<RemoteArchive> = parse_apache_index(html)
        .into_iter()
        .filter_map(|entry| {
            // Cross-source names, sort links, and malformed names all fail to
            // parse and are dropped here — never downloaded — identically to the
            // on-disk planner's rejection.
            let parsed = ParsedArchive::parse_file_name(source, &entry.name).ok()?;
            Some(RemoteArchive {
                parsed,
                size: entry.size,
                last_modified: entry.last_modified,
            })
        })
        .collect();

    archives.sort_by(|left, right| {
        left.parsed
            .timestamp
            .cmp(&right.parsed.timestamp)
            .then_with(|| left.parsed.file_name.cmp(&right.parsed.file_name))
    });
    archives
}

/// Reduce an `href` value to a bare candidate file name, or `None` if it is a
/// directory, navigation, sort, or absolute-URL link we must not treat as a
/// downloadable archive.
fn normalise_href(href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() {
        return None;
    }
    // Column-sort links (`?C=N;O=D`) and fragments.
    if href.starts_with('?') || href.starts_with('#') {
        return None;
    }
    // Parent-directory and absolute paths / URLs.
    if href == ".." || href == "../" || href.starts_with('/') {
        return None;
    }
    if href.contains("://") {
        return None;
    }
    // Directory entries end in `/`.
    if href.ends_with('/') {
        return None;
    }
    // Keep only the final path component.
    let name = href.rsplit('/').next().unwrap_or(href);
    let name = percent_decode_minimal(name);
    if name.is_empty() {
        return None;
    }
    Some(name)
}

/// Decode the handful of percent-escapes Apache emits in hrefs (it leaves most
/// alphanumerics and `_`/`-`/`.` untouched, which covers every DILA archive
/// name). This is deliberately minimal — DILA archive names are ASCII.
fn percent_decode_minimal(value: &str) -> String {
    if !value.contains('%') {
        return value.to_owned();
    }
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx] == b'%' && idx + 2 < bytes.len() {
            let hi = (bytes[idx + 1] as char).to_digit(16);
            let lo = (bytes[idx + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push(((hi * 16 + lo) as u8) as char);
                idx += 3;
                continue;
            }
        }
        out.push(bytes[idx] as char);
        idx += 1;
    }
    out
}

/// From the HTML immediately following an anchor, scrape the (date, size)
/// columns up to the next row/line boundary. Both columns are optional.
fn scrape_tail(after: &str) -> Option<(String, Option<String>)> {
    // Bound the search to the current row / pre-line so we never pick up a
    // later file's columns.
    let boundary = after
        .find("</tr>")
        .or_else(|| after.find("<tr"))
        .or_else(|| after.find('\n'))
        .unwrap_or(after.len());
    let window = &after[..boundary.min(after.len())];
    let text = strip_tags(window);
    let captures = TAIL_RE.captures(&text)?;
    let date = captures.name("date")?.as_str().trim().to_owned();
    let size = captures
        .name("size")
        .map(|m| m.as_str().trim().to_owned())
        .filter(|s| !s.is_empty() && s != "-");
    Some((date, size))
}

/// Crude tag stripper for a single row/line: replaces tags with spaces so the
/// fixed-width date/size text survives intact.
fn strip_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}
