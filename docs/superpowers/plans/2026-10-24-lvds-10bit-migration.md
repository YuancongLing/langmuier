# LVDS 10bit Protocol Migration Implementation Plan

**Goal:** Replace live LVDS decoding with the manufacturer 04/FF, 269-byte, dual 100-point 10bit protocol and deliver native Windows CSV/LPD v3 capture.

**Architecture:** Keep UDP framing, source filtering, TTL, dedup and u32 sequence observation unchanged. The scientific model stores exact u16 code10 arrays and the inner frame_seq_u8; signed reconstruction is derived. CSV and an explicitly versioned binary v3 sink consume Accepted frames through the existing bounded BatchWriter. Isolate legacy v2 data from the new scientific model; disable the legacy HDF5 UI path for new frames.

**Tech Stack:** Rust std, existing crates and egui/eframe; no new dependencies.

**Spec:** User attachment `C:/Users/Administrator/.codex/attachments/3a4cdffe-d768-4c6a-88b3-70d23d772647/Pasted text.txt` (the attachment contains text only, including the complete field table and vectors).

## Global constraints

- Exact new addresses 04/FF, TYPE AA, LEN 0107, DATA251=00; no automatic protocol guessing or 55/AA fallback.
- Two separate 125-byte blocks decode to 100 code10 values each; interval 500 us, first sample has the transmitted timestamp without another 200 us.
- Preserve received 10bit codes; reconstructed ADC is `(code << 6) - 32768`, signed, uncalibrated. No old mV conversion.
- CRC remains unchecked in production, with a visible notice. Preserve outer UDP u32 monitoring and distinct inner seq8; never synthesize legacy u16 sequence.
- Preserve RS422, UDP outer bytes/TTL/dedup/sequence algorithms, plot interactions and bounded storage. Old PCAPs stay read-only. No hardware connection or Git commit/push.
- Latest real new-protocol capture is not supplied; golden fixtures are explicitly synthetic, and real new-device acceptance is NOT_RUN.

## Review focus

- Every 5-byte boundary and two channel boundaries must retain all 100 code10 values; test literal golden vectors and distinct channels.
- Inner seq8 wrap must not replace full outer u32 dedup/sequence keys; exercise wraps and identical inner values with distinct outer keys.
- Time near a second rollover and device rollback must retain original values and write order; test first sample and final sample without 200 us duplication.
- v2 and v3 files must be unambiguous; retain v2 layout tests, reject mismatched versions and truncated v3 records, no fake padding.
- Both-file failures, EOF/manual stop and chart congestion must retain true write counts with bounded queues; migrate existing lifecycle tests rather than remove them.

## Tasks

- [x] Core/transport/CLI: first add failing literal vectors `FF C0 08 00 01 -> [1023,0,512,1]` and `00 3F F0 03 FF -> [0,1023,0,1023]`, fixed fields, reserved byte and time tests. Implement `ScientificFrame { timestamp, frame_seq_u8: u8, channel_1/2: Box<[u16;100]> }`, `unpack_4x10`, signed `reconstruct_adc16`, integer `sample_time_us`; migrate core and transport fixtures while preserving outer algorithms. Run core/transport/CLI tests.
- [x] Storage: preserve v2 constants/layout with `LegacyScientificFrameV2`, independent of the new core count. Add a separate v3 writer/reader with explicit representation/count/interval/sequence metadata and malformed-file rejection. Migrate scientific CSV to the requested 11 columns/100 rows per frame; test signed values, seq8 wrap, streaming, I/O errors and readback. Retain HDF5 legacy schema, do not feed new frames to it.
- [x] UI: write failing sample/axis tests, migrate 2000 Hz/120100-point history and 500 us fixtures, display reconstructed ADC without mV, show device seconds without calendar-epoch assumption, retain all existing plot interactions. Disable new-data HDF5 with a reason. Update mock fixture only, never formal backend generation.
- [x] Backend/integration: use v3 for new LPD, derive signed plot values from exact codes and retain first-sample timestamp. Update golden ingress, disk readback, CSV modes, backpressure, EOF/stop and error tests; verify old captures rejected. Construct an independently packed test PCAP for non-Mock replay verification, clearly marked synthetic.
- [x] Verification/delivery: fmt/check/workspace tests, strict Clippy with baseline distinction, default release plus documented Windows x64 static CRT non-Mock build. Native UI replay/screenshot and independent byte/CSV/v3 readback; rebuild and verify EXE/ZIP. Preserve old PCAP hashes.
- [x] Report: `docs/LVDS_10BIT_PROTOCOL_MIGRATION_QA.md` with layouts, vectors, signed quantization limits, time semantics, v2/v3/HDF5 boundaries, real-device NOT_RUN, exact checks and change list. Stop at this scope, no Git commit.

Execution uses parallel component work with shared interface contracts and a final independent review, as authorized by the implementation request. The plan is self-reviewed against all 16 specification sections.

Final evidence: 276 passed / 0 failed / 2 ignored; both non-Mock releases built; native synthetic replay and complete CSV/v3 readback verified; 65-second native 2kHz replay stayed within cache/render bounds; EXE/ZIP byte and PE checks passed. Strict Clippy remains baseline FAIL; optional HDF5 and real new-device acceptance remain NOT_RUN. No Git commit/push.
