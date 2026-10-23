# LVDS Scientific CSV Implementation Plan

**Goal:** Stream all Accepted LVDS dual-channel raw ADC samples to CSV, LPD, or both from the real backend.

**Architecture:** Reuse the existing bounded BatchWriter with a buffered ScientificCsvSink and one composite scientific sink. UI selects a fixed capture format; PCAP replay optionally creates the writer before delivering its first frame. Independent per-file progress reports sink writes, close and failures separately from queue submissions.

**Tech Stack:** Rust standard I/O, existing crossbeam/parking_lot, egui/eframe; no new dependency.

**Spec:** User attachment `C:/Users/Administrator/.codex/attachments/8739fe21-95b8-432f-92e9-b9ac7c3f557a/Pasted text.txt`.

## Constraints

Preserve RS422, UDP 24B envelope, LVDS 269B parser/ADC, CRC admission rules, 2s TTL, 5s/256 dedup, sequence monitoring, LPD v2, HDF5 schema, plots and Sidecar. No automatic hardware connection. The initial task prohibited automatic Git commits; after acceptance, the user explicitly requested updating GitHub and authorized the subsequent commit and push. Final package is native Windows x64 non-Mock.

## Review Focus

- Peer-file write/flush failure: stop incomplete, retain actual written data and separate file counters.
- PCAP first-frame and EOF boundaries: writer before delivery, graceful automatic close.
- Timestamp rollback, max u32 seconds and sequence wrap: preserve accepted order and integer values.
- Long capture: bounded queue and per-frame encoding, incremental disk writes.
- Low-resolution toolbar and save lock: no new overall scroll or growing central controls.

## Tasks

- [x] Storage: add `crates/langmuir-storage/src/scientific_csv.rs`, export config/sink/error/header in lib.rs. Add failing then passing tests for 125/12500 rows, paired ADC, time rollover, u16 wrap, backward time, create_new, I/O and close. CSV header exactly matches the spec; use u64 integer microseconds and CRC_NOT_CHECKED.
- [x] Backend/bridge: introduce `ScientificSaveFormat`, format on StartAcquisition and optional format on StartPcapReplay. Add one composite sink with per-file progress. Add lifecycle, modes, first-frame, manual-stop, malformed input/dedup and failure tests. Keep BatchWriter and existing protocols unchanged.
- [x] UI/mock: add compact format selector locked while saving and replay-save checkbox; distinguish science CSV from telemetry CSV. Display queue counts, actual per-file frames/CSV rows, full paths and file state. Test request interactions and existing layout sizes.
- [x] Acceptance: real PCAP independent full CSV/ADC readback plus LPD v2 regression, first/last/random/rollback checks and bounded long capture. Preserve source hash. Final input is user-selected `102301.pcapng`; no actual rollback in this file, covered separately by deterministic tests. Original requested capture unavailable, recorded in QA.
- [x] Regression: cargo fmt --all -- --check; cargo check --workspace --locked; cargo test --workspace --features langmuir-desktop/mock --locked; strict Clippy without lint relaxation; cargo build --release -p langmuir-desktop --locked. Record failures with baseline distinction.
- [x] Windows: follow docs/WINDOWS_BUILD.md static-CRT native x64 non-Mock build, launch real UI, capture screenshots and replay where source exists. Rebuild ZIP with EXE and instructions; verify ZIP bytes, PE x64 and CRT imports.
- [x] Report: docs/LVDS_SCIENTIFIC_CSV_QA.md with schema, lifecycle/error policy, exact evidence, screenshots, Git file list and hardware NOT_RUN items. Subsequent GitHub update follows the user's explicit authorization; raw captures and local build/data outputs remain excluded.
