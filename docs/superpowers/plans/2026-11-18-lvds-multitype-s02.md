# LVDS MULTITYPE S02 Implementation Plan

> **For agentic workers:** Execute the bounded components below in parallel with test-first verification. User authorized implementation and prohibits automatic Git commits.

**Goal:** Decode AB excitation frames, show their independent 100-point waveform, save an independent CSV, and browse/export AB history while preserving S01.

**Architecture:** Reuse the common LVDS parser and its auxiliary frame gate. Accepted AB frames enter an independent bounded backend business queue, then BatchWriter; visual events never drive storage. History scans and navigation run on workers with bounded retained records.

**Tech Stack:** Rust workspace, egui/egui_plot, BatchWriter, standard-library Python reference encoder, Windows MSVC static CRT.

**Spec:** `C:/Users/Administrator/.codex/attachments/f9958150-dcec-4aef-9113-debb4250fd7d/Pasted text.txt`; `协议 (PH20261115).xlsx`.

## Global Constraints

- Preserve all uncommitted changes, S01, star simulator, AA CSV 11 columns and LPD v3 40B/409B.
- No AC business decoding, new socket, automatic device connection, RS422 command, Git commit, or ZIP delivery.
- AB DATA offsets follow Excel I17:J31. Timestamp is wave[99]; checked u64 subtraction uses interval_us. Do not concatenate overlapping frames.
- CRC retained but not validated; all-zero/frequency-zero valid structure is validity unconfirmed.
- Excel J24 scan_freq 0..100 applies triangle/sine; J25 DC voltage 0..5000 and other modes 0. interval_us has no nonzero constraint.

## Review Focus

- A burst exceeding the 128 visual queue must still reach independent CSV, with business/storage overflow counters when their own bounds are exceeded.
- Source/destination transitions and fragment expiry must not join unrelated fragments or save duplicates.
- Adjacent AB windows may overlap; preserve arrival order in CSV and freeze the selected frame in manual plot view.
- History scan, window reads, exports, cancellation and navigation must consume/clear bounded AB queues.
- CSV create/write/flush/close failures must remain visible; existing files must never be overwritten.

## Task 1: Core decoder and independent CSV (core/storage agent)

**Files:** new core excitation module, new storage excitation CSV module, lib exports; `tools/qa/lvds_multitype_s02_fixture.py`, fixed vectors and core/storage tests.

**Interfaces:** `ExcitationFrame { timestamp: Timestamp, f_act_mhz:u32, interval_us:u16, points:u16, work_mode:u8, scan_freq:u8, scan_voltage_mv:u16, ad9833_status:u8, dac8563_status:u8, sci_frame_seq:u8, pa0_seq:u8, wave_mv:Box<[u16;100]> }`; `decode_excitation_frame(&LvdsCommonFrame)->Result<ExcitationFrame,ExcitationError>`; `sample_time_us(usize)->Option<u64>`, `actual_frequency_hz()->f64`, `is_all_zero()->bool`. `ExcitationRecord { frame, outer_sequence:u32, raw_record_id:u64, source:String, session:String, received_unix_nanos:u64 }`. `ExcitationCsvConfig::new`, `ExcitationCsvSink::create`, `BatchSink<ExcitationRecord>`, rows/frames counters.

- [x] Generate independent synthetic bytes for three modes, 0/3300mV, 931mHz, 10800us, seq wrap, zero data and arbitrary CRC. Add Rust tests reading fixed bytes and literal expected sample times.
- [x] Run decoder tests RED; implement validation, checked time calculations, then run GREEN.
- [x] Add CSV tests for 100 rows/frame, exact mV/V/time/metadata, overlaps/order and exclusive creation; run RED, implement buffered sink, run GREEN.

## Task 2: Ingress, reliable storage and bridge (root)

**Files:** `backend.rs`, `bridge.rs`, new `backend_excitation.rs`, backend integration tests.

**Interfaces:** ingress `excitation_records:VecDeque<ExcitationRecord>` bounded 1024; `excitation_errors:VecDeque<ExcitationDecodeFailure>` bounded 128. Extend receive stats with AB decoded/error/duplicate/conflict/business-drop counters. Bridge `ExcitationDecodeFailure {raw_record_id,outer_sequence,reason}`; `ExcitationStorageStatus {active,path,enqueued,written,rows_written,dropped,closed,error}`. `LvdsStatusSnapshot.excitation_latest` and `.excitation_storage`. Event `ExcitationFrame(Box<ExcitationRecord>)`. Requests `StartExcitationCsv{path}`, `StopExcitationCsv`, `LoadHistoryExcitationFrame{index}`, `ExportHistoryExcitationCsv{path}`.

- [x] Add tests proving decoded/duplicate/conflict/error behavior and mixed20AA+1AB+1AC counts, no false gaps, 100 AB plot samples/CSV rows, missing/swapped auxiliary independence.
- [x] Run RED; route accepted AB using existing gate, independent business queue and bounded errors; drain business data before EOF storage stop.
- [x] Implement tracked BatchWriter sink with nonblocking submit, reliable counters and error/stop lifecycle; add overload and exclusive-create tests, run GREEN.

## Task 3: Bounded history (history agent)

**Files:** `backend_history.rs`, new history excitation module/tests.

**Interfaces:** `HistoryStatus.excitation:ExcitationHistoryStatus {frames:u64, errors:Vec<ExcitationDecodeFailure>, selected_index:Option<u64>, selected:Option<ExcitationRecord>, loading:bool, exporting:bool, export_path:Option<String>, export_error:Option<String>}`; commands `ExcitationFrame(u64)`, `ExportExcitation{path:String}`. Separate AB cancellation epoch.

- [x] Add mixed RAW/PCAP tests for AB count, selection/previous/next, invalid raw ID, CSV export and cancellation; run RED.
- [x] Scan/count on worker, retain bounded record/error page; on-demand rescan selection with full framing/dedup context. Consume business queue after each raw input in every scan path.
- [x] Export accepted AB in arrival order with independent CSV sink and cancellation; run GREEN and regression history tests.

## Task 4: Independent AB GUI (UI agent)

**Files:** new `app_excitation.rs`; `app.rs`, `app_history.rs` integration and tests.

- [x] Add tests for latest full-frame points, mV/1000, latest-point time, AA/AB state independence, freeze/manual updates, bounded history selection; run RED.
- [x] Add compact AA/AB view selector within existing central toolbar; separate plot ID/state, follow/reset/freeze/pan/zoom/point values. Preserve frame while manual/frozen. Show actual metadata including raw 0/1 states, frequency Hz and raw provenance.
- [x] Add independent CSV controls/status and history frame navigation/export/error details using bridge interfaces; run GREEN.

## Task 5: Acceptance and delivery (root + review)

- [x] Review integrated changes and exercise independent fixture/CSV checker, non-Mock replay, real AA capture regression and RAW history.
- [x] Run `cargo fmt --all -- --check`; `cargo check --workspace --locked`; `cargo test --workspace --features langmuir-desktop/mock --locked`; `cargo clippy --workspace --all-targets --locked -- -D warnings`.
- [x] Build static CRT non-Mock Windows x64 release; verify PE/imports/hash; preserve S01 and deliver separate S02 EXE.
- [ ] Native Windows GUI screenshots/interactions NOT_RUN: current session exposes no node_repl/Computer Use tool; actual AB hardware acceptance BLOCKED: no real AB input supplied. egui event tests and backend CSV readback pass but do not replace native GUI verification.
- [x] Write `docs/LVDS_MULTITYPE_S02_EXCITATION_ACCEPTANCE.md` with field/cell mapping, evidence, test totals, git list, limitations and usage. No automatic commit.

Self-review: all thirteen specification sections map to tasks above; interfaces agree; all five review inputs have tests assigned. Execution continues autonomously under developer instructions.
