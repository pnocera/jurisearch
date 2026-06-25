//! Shared archive-run member batching: the per-archive read+batch+flush loop that the LEGI and
//! JURI ingest payloads both run. ONLY the batching mechanics live here — each payload keeps its
//! own open/start/finish/manifest/run-status-recompute/backfill/response so the ingestion
//! accounting contract (when a run starts, when the manifest is written, when `run_status` is
//! recomputed, when the replay snapshot refreshes) stays explicit and auditable in the two
//! payloads rather than hidden behind a generic runner (codex design call: extract the loop, not
//! an `ArchiveIngestAdapter` trait over the heterogeneous counter types).

use jurisearch_ingest::archive::ArchiveReadError;

use crate::*;

/// Outcome of reading ONE planned archive through the batching loop.
pub(crate) struct ArchiveBatchReadReport {
    /// The running member-visit count AFTER this archive. The caller writes it back into its
    /// source-specific counters; `visited_members` is deliberately not part of the per-batch
    /// `merge_committed` accounting, which is why it is threaded through this helper by value.
    pub(crate) visited_members: usize,
    /// True when the `--limit-members` cap was reached, so the caller stops the archive loop.
    pub(crate) stopped_by_limit: bool,
}

/// Why reading ONE archive stopped with an error. Both variants carry the running visited count so
/// the caller can keep its accounting consistent before converting the error into an `ErrorObject`.
pub(crate) enum ArchiveBatchReadError {
    /// A batch flush transaction failed (mid-archive on an overflow/size flush, or on the tail
    /// flush). Earlier flushes in this archive already committed; the current pending batch is
    /// abandoned (mirrors the original payloads).
    Flush {
        visited_members: usize,
        error: StorageError,
    },
    /// `for_each_xml_member_until` failed to read the archive itself. Any pending (un-flushed)
    /// members are dropped — the tail flush is skipped on a read error, exactly as before.
    Read {
        visited_members: usize,
        error: ArchiveReadError,
    },
}

/// Read ONE archive, batching its XML members and flushing each full batch through `flush`. Owns the
/// batching mechanics only: the pending buffer, the count/byte thresholds, flush-before-overflow,
/// the tail flush, and the `--limit-members` stop. The caller owns everything else (run lifecycle,
/// counters, read-error message text, response).
///
/// `visited_members` is threaded by value in and returned out (rather than read from the caller's
/// counters) so `flush` can freely capture `&mut counters` for the source-specific committed counts
/// without a borrow conflict on the visit count — `visited_members` is never touched by a flush.
///
/// Contract: on success `flush` MUST drain `pending_members` and reset the byte counter to 0 (as
/// `flush_legi_archive_member_batch` / `flush_juri_archive_member_batch` do) — this helper does not
/// clear the buffer itself, it only decides when to call `flush`.
pub(crate) fn read_archive_members_batched<F>(
    archive_path: &Path,
    max_member_bytes: u64,
    limit_members: Option<usize>,
    visited_members: usize,
    mut flush: F,
) -> Result<ArchiveBatchReadReport, ArchiveBatchReadError>
where
    F: FnMut(&mut Vec<ArchiveMember>, &mut usize) -> Result<(), StorageError>,
{
    let mut pending_members = Vec::with_capacity(LEGI_INGEST_TRANSACTION_BATCH_SIZE);
    let mut pending_member_bytes = 0usize;
    let mut visited = visited_members;
    let mut flush_error = None::<StorageError>;

    let read_result = for_each_xml_member_until(archive_path, max_member_bytes, |member| {
        if limit_members.is_some_and(|limit| visited >= limit) {
            return Ok(ArchiveVisit::Stop);
        }
        visited += 1;
        let member_bytes = member.bytes.len();
        if !pending_members.is_empty()
            && pending_member_bytes.saturating_add(member_bytes)
                > LEGI_INGEST_TRANSACTION_BATCH_BYTE_LIMIT
            && let Err(error) = flush(&mut pending_members, &mut pending_member_bytes)
        {
            flush_error = Some(error);
            return Ok(ArchiveVisit::Stop);
        }
        pending_members.push(member);
        pending_member_bytes = pending_member_bytes.saturating_add(member_bytes);
        if (pending_members.len() >= LEGI_INGEST_TRANSACTION_BATCH_SIZE
            || pending_member_bytes >= LEGI_INGEST_TRANSACTION_BATCH_BYTE_LIMIT)
            && let Err(error) = flush(&mut pending_members, &mut pending_member_bytes)
        {
            flush_error = Some(error);
            return Ok(ArchiveVisit::Stop);
        }
        Ok(if limit_members.is_some_and(|limit| visited >= limit) {
            ArchiveVisit::Stop
        } else {
            ArchiveVisit::Continue
        })
    });

    // Tail flush only when the read finished cleanly AND no flush already failed: a read error or a
    // mid-archive flush failure abandons the remaining pending members (preserves the original).
    if flush_error.is_none()
        && read_result.is_ok()
        && !pending_members.is_empty()
        && let Err(error) = flush(&mut pending_members, &mut pending_member_bytes)
    {
        flush_error = Some(error);
    }

    // Precedence: a flush failure (which short-circuits the read via `Stop`, so `read_result` is
    // `Ok`) takes priority over a read failure, matching the original `fatal_error`-then-read order.
    if let Some(error) = flush_error {
        return Err(ArchiveBatchReadError::Flush {
            visited_members: visited,
            error,
        });
    }
    if let Err(error) = read_result {
        return Err(ArchiveBatchReadError::Read {
            visited_members: visited,
            error,
        });
    }
    Ok(ArchiveBatchReadReport {
        visited_members: visited,
        stopped_by_limit: limit_members.is_some_and(|limit| visited >= limit),
    })
}

#[cfg(test)]
mod tests {
    use flate2::Compression;
    use flate2::write::GzEncoder;

    use super::*;

    /// Build a gzipped tar of `count` tiny `.xml` members (the only members `for_each_xml_member_until`
    /// yields) so the batching helper can be exercised without an archive on disk shaped by ingestion.
    fn write_xml_archive(count: usize) -> tempfile::NamedTempFile {
        let file = tempfile::Builder::new()
            .suffix(".tar.gz")
            .tempfile()
            .expect("tempfile");
        let encoder = GzEncoder::new(Vec::new(), Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for index in 0..count {
            let body = b"<x/>";
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, format!("member-{index:05}.xml"), &body[..])
                .expect("append member");
        }
        let bytes = builder
            .into_inner()
            .expect("finish tar")
            .finish()
            .expect("gzip");
        std::fs::write(file.path(), bytes).expect("write archive");
        file
    }

    /// A flush closure that records each batch size and honours the drain contract; optionally fails
    /// on the Nth flush (1-based) to exercise the flush-error path.
    fn recording_flush(
        sizes: &mut Vec<usize>,
        fail_on: Option<usize>,
    ) -> impl FnMut(&mut Vec<ArchiveMember>, &mut usize) -> Result<(), StorageError> + '_ {
        let mut calls = 0usize;
        move |pending, bytes| {
            calls += 1;
            sizes.push(pending.len());
            pending.clear();
            *bytes = 0;
            if fail_on == Some(calls) {
                return Err(StorageError::MissingHome);
            }
            Ok(())
        }
    }

    #[test]
    fn small_archive_flushes_the_tail_once_and_returns_visited() {
        let archive = write_xml_archive(3);
        let mut sizes = Vec::new();
        let report = read_archive_members_batched(
            archive.path(),
            u64::MAX,
            None,
            0,
            recording_flush(&mut sizes, None),
        )
        .unwrap_or_else(|_| panic!("clean read"));
        assert_eq!(report.visited_members, 3);
        assert!(!report.stopped_by_limit);
        // One tail flush of all three members; nothing flushed mid-stream (3 < batch size).
        assert_eq!(sizes, vec![3]);
    }

    #[test]
    fn flushes_a_full_batch_then_the_tail() {
        // One member past a full batch: a count-triggered flush of LEGI_INGEST_TRANSACTION_BATCH_SIZE
        // then a tail flush of the remaining member.
        let archive = write_xml_archive(LEGI_INGEST_TRANSACTION_BATCH_SIZE + 1);
        let mut sizes = Vec::new();
        let report = read_archive_members_batched(
            archive.path(),
            u64::MAX,
            None,
            0,
            recording_flush(&mut sizes, None),
        )
        .unwrap_or_else(|_| panic!("clean read"));
        assert_eq!(
            report.visited_members,
            LEGI_INGEST_TRANSACTION_BATCH_SIZE + 1
        );
        assert!(!report.stopped_by_limit);
        assert_eq!(sizes, vec![LEGI_INGEST_TRANSACTION_BATCH_SIZE, 1]);
    }

    #[test]
    fn stops_exactly_at_the_member_limit_and_carries_the_running_count() {
        let archive = write_xml_archive(10);
        let mut sizes = Vec::new();
        // Start the running count at 2 (as if a prior archive visited two members): the limit is
        // global across archives, so this archive should visit only 3 more to reach 5.
        let report = read_archive_members_batched(
            archive.path(),
            u64::MAX,
            Some(5),
            2,
            recording_flush(&mut sizes, None),
        )
        .unwrap_or_else(|_| panic!("clean read"));
        assert_eq!(report.visited_members, 5);
        assert!(report.stopped_by_limit);
        assert_eq!(sizes, vec![3]);
    }

    #[test]
    fn flush_failure_reports_flush_error_with_the_visited_count() {
        let archive = write_xml_archive(3);
        let mut sizes = Vec::new();
        let error = read_archive_members_batched(
            archive.path(),
            u64::MAX,
            None,
            0,
            recording_flush(&mut sizes, Some(1)),
        )
        .err()
        .expect("flush failure surfaces as an error");
        match error {
            ArchiveBatchReadError::Flush {
                visited_members,
                error,
            } => {
                assert_eq!(visited_members, 3);
                assert!(matches!(error, StorageError::MissingHome));
            }
            ArchiveBatchReadError::Read { .. } => {
                panic!("expected a flush error, not a read error")
            }
        }
    }
}
