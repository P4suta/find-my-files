# ADR-0011: streaming scan pipeline (I/O multiplexing rejected)

Date: 2026-06-11 / Status: Accepted

## Decision

The initial scan reads $MFT as buffered synchronous streaming in 16MiB chunks; a single dedicated I/O thread prefetches chunk N+1 (3 buffers fix the RAM ceiling), and within a chunk rayon parses 1MiB record-boundary subranges in parallel. Name resolution for $ATTRIBUTE_LIST (deferred) RAM-caches the extension records that carry $FILE_NAME during streaming (capped at 128Ki entries ≈ 128MiB temporary) and runs with zero disk reads. I/O multiplexing via NO_BUFFERING + overlapped is not adopted.

## Rationale

- Measured deferred path: 2.9s with the disk-read version → **8ms** with the RAM cache. Random reads on `\\.\C:` are serialized in the kernel regardless of the number of outstanding handles, so they do not shrink with parallel I/O
- Whole scan 5.0s→2.1s (read at 1.6s is the limiter)
- `fmf io-probe C:` ($MFT 1.54GiB) measured: buffered sync 962.6 / +SEQUENTIAL_SCAN 960.9 / NO_BUFFERING sync 958.2 / NO_BUFFERING+overlapped QD2 1075.9 / QD4 **1101.6 MB/s (+14.4%)**. Below the adoption bar (read +30% for Stage 2; adopt at whole-scan −25%). The whole-scan effect is projected at 2.0→~1.85s, not worth it given the current state already clearing the M2 scan gate (60s) by 30×
- EntryId assignment appends worker batches in chunk order, so it matches the sequential version deterministically (admin test `streaming_scan_matches_reference` is the equivalence gate)

## Impact

- Temporary RAM: 3×16MiB pipeline buffers + extension-record cache (cap 128Ki entries). Overflow increments the `ext_name_cache_skipped` counter + falls back to disk
- I/O thread startup failure increments the `scan_pipeline_fallbacks` counter + degrades to sequential reads (does not stay silent)
- `fmf io-probe` is kept on hand as a measurement tool

## Re-examination trigger

- If a multi-volume concurrent-scan requirement arises, or the scan gate is tightened to 10s or below (re-evaluate Stage 2 overlapped multiplexing)
