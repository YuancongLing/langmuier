# LVDS MULTITYPE S01 Implementation Plan

**Goal:** Receive and preserve AA, AB and AC through one production UDP pipeline while keeping AA science decoding, display and storage intact.

**Architecture:** Add a common 269-byte frame model and bounded byte synchronization ahead of the existing science path. Auxiliary frames have their own bounded observations and history metadata; raw recording remains before source filtering and protocol parsing. Outer u32 continuity observes the entire accepted stream independently of science seq8.

**Tech Stack:** Rust workspace, crossbeam bounded queues, Windows x64 MSVC/static CRT, existing LVDSRAW v1 and LPD v3.

**Spec:** User-supplied LVDS-MULTITYPE-S01 acceptance requirements; `协议 (PH20261115).xlsx`, sheets `LVDS_公共格式`, `LVDS_科学数据包`, `LVDS_激励信号数据包`, `LVDS_GNSS数据包`.

## Constraints

- Preserve all pre-existing uncommitted edits and original captures/reports. Do not commit Git.
- Fixed 269B: EB90 / SRC55 / DSTAA / LEN0107 / TYPE / DATA260 / CRC2. Retain CRC bytes; production CRC is not checked.
- Keep the existing AA 10bit decoder, recovery quality gates, 100 samples/channel, 500us, CSV and 40B/409B LPD v3 format.
- No AB waveform decoding, AC parameter decoding, RS422 encoding changes or three-view UI redesign.
- All mixed fixtures must be labelled SYNTHETIC; real AA captures do not establish real FPGA AB/AC acceptance.

## Review focus

- A valid auxiliary frame must never expose embedded bytes to the AA decoder or bit recoverer.
- Unknown types and bad/short/misaligned/duplicate/wrong-source packets remain byte-exact in raw storage.
- Interleaved auxiliary packets do not create outer sequence gaps or change AA seq8.
- Fragment counts, complete-frame counts and actual raw write counts remain distinct.
- Historical auxiliary-only files must retain frames and counts without fabricated science or serial sends.

## Tasks

1. Common parser and synchronization (`crates/langmuir-core/src/lvds_common.rs`, `lvds.rs`, `lvds_recovery.rs`, `lib.rs`). First add failing tests for AA/AB/AC/unknown common parsing, malformed fixed fields, CRC passthrough and bounded unique byte synchronization. Implement `LvdsPacketType`, `LvdsCommonFrame`, `parse_common_frame` and `find_common_frame`; retain the science-only bit recovery algorithm.
2. Production routing (`apps/desktop/src/backend.rs`, `bridge.rs`, new `backend_multitype_tests.rs`). First reproduce AB/AC science rejection and false sequence gaps. Branch on common TYPE before science recovery; retain full frames with source, raw record dependencies, outer sequence and capture time. Add common duplicate/conflict tracking and whole-stream sequence observation while keeping old science counters/handlers. Test 20+1+1, missing auxiliaries, arbitrary order, fragments, unknowns, conflicts and auxiliary recovery exclusion.
3. Raw/history integration (`backend_history.rs`, new multitype raw/history tests). Use the same ingress for PCAP/LVDSRAW; expose bounded full auxiliary records and receive counts in `HistoryStatus`. Pass original capture timestamps. Test localhost UDP -> actual LVDSRAW close/read with exact payload equality, mixed history and auxiliary-only history. Reuse existing storage failure/overflow tests without redesigning the archive.
4. Minimal UI (`app.rs`, `app_history.rs`). Show type/structure/raw details in existing more/history panels, with AB/AC marked received/pending decoding. Verify existing headless layout/plot/storage tests.
5. Verification and delivery. Run requested fmt/check/workspace tests/clippy, available real-AA ignored regressions, then rebuild non-Mock static-CRT Windows release using `docs/WINDOWS_BUILD.md`. Verify PE/imports, binary hashes and package contents. Write `docs/LVDS_MULTITYPE_S01_ACCEPTANCE.md` with PASS/FAIL/BLOCKED/NOT_RUN and explicit real-FPGA limits.

## Execution checklist

- [x] Audit initial Git status/diff and preserve user changes.
- [x] Read Excel protocol directly and audit ingress/raw/history boundaries.
- [x] Observe targeted failing tests before production changes.
- [x] Implement and independently review common/routing/raw/history/UI changes.
- [x] Run requested automated checks and real AA regressions.
- [x] Build and verify Windows non-Mock package.
- [x] Complete acceptance report with actual evidence and remaining hardware limits.
