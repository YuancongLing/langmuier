# Raw capture, bounded recovery and history implementation plan

> **For agentic workers:** Implement each owned component with a failing behavioral test, minimal implementation and fresh verification. Root integrates the production path; independent workers audit raw storage, history and real input evidence. Do not commit Git.

**Goal:** Archive received UDP payloads unchanged and open raw/LPD/PCAP histories in the Windows desktop, with optional, traceable bit alignment recovery.

**Architecture:** A separate bounded raw writer observes each UDP datagram before filtering. Existing science decoding and deduplication remain the native acceptance path. A bounded recovery module supplies uniquely aligned frames or diagnostic candidates, with independent audit and quality-aware export. A background history worker builds a capped sparse file index and streams only the requested time window into the existing plot.

**Tech Stack:** Rust 2024 / MSRV 1.92, crossbeam-channel, existing BatchWriter, eframe/egui_plot, SHA-256 and JSONL audit, Python independent reference tools, Windows MSVC static CRT.

**Spec:** User attachment `C:/Users/Administrator/.codex/attachments/05a27269-9534-484d-b508-9c0b8ffbd8e6/Pasted text.txt`.

## Global constraints

- Product name remains `Langmuier Probe Ground Control`.
- Native science remains 55/AA, 269B, 100 code10 per channel, 500us; ordinary production frames never use CRC as a gate.
- RS422, its commands, legacy formats, standard CSV 11 columns and LPD v3 40B/409B layout stay compatible.
- Raw bytes and CRC tails are never modified, padded, interpolated or overwritten.
- Recovery is off by default. There is no cross-record bit concatenation without independent bit-continuity evidence; this implementation uses none.
- Only native frames enter existing LPD/standard CSV. Recovered and candidate samples use explicit quality and independently versioned export.
- Bounded queues, maximum recovery payload, bounded sparse index, cancellable readers and 60-second plot windows prevent unbounded memory growth.
- No automatic hardware connection, RS422 command transmission, deletion of user data or Git commit.
- Use actual `2026102401.pcapng` and `lvds.txt` if available. Missing capture is not replaced by synthetic data. Synthetic stress fixtures are named accordingly.

## Review focus

1. Process interruption exactly between complete raw records: missing mandatory footer still means incomplete.
2. Wildcard live socket binding is not the packet's actual destination: record endpoint semantics explicitly.
3. An apparently aligned 269B input can lack final bits after shifting: diagnostics may use only real available prefix, never manufacture the tail.
4. Seeking or switching files while reader/export is active: cancellation, session reset, clear plot window, no stale delivery or accidental hardware operation.
5. A very long input or malformed length: cap allocations/index/queues/search, stream export independently of display decimation.

## Production chain and ownership

Before: `UdpLvdsReceiver::drain / PcapReplay → source filter → parse_udp_lvds_datagram → UdpLvdsReassembler → UdpLvdsScienceGate → Controller::handle_scientific → plot + native storage`.

After: observe raw UDP before source filtering; bounded reassembly → native decode / unique byte-sync / optional recovery → quality-aware display and audit. Native gate and native storage retain their existing semantics. Raw and historical readers share this ingress; LPD uses BinaryV3Reader directly.

### Task 1: Versioned raw archive (storage worker)

**Files:** `crates/langmuir-storage/src/raw.rs`, `raw_tests.rs`, optional `raw_archive.rs`, exports in `lib.rs`, `docs/LVDSRAW_FORMAT.md`.

**Interfaces:** `RawOrigin::{LiveUdp,PcapReplay}`; `RawDestination::{Captured(SocketAddr),SocketBinding(SocketAddr),Unknown}`; `RawRecord { id:u64, timestamp:Duration, source:SocketAddr, destination:RawDestination, source_matches:bool, origin_packet_index:Option<u64>, payload:Vec<u8> }`; `RawHeader { origin, created_at, expected_source, binding, session_id:[u8;16] }`; `RawArchive::{start,try_record,status,stop}`; `RawReader::{open,new,header,next_record,summary}`. `try_record` returns a receipt containing the monotonically assigned id and queue result; overflow permanently marks the archive incomplete.

- [x] RED: independent literal header/record read, exact byte round trip including invalid/duplicate UDP, exclusive create, zero-record explicit close.
- [x] RED: entire-record-boundary interruption, partial head/record/footer, impossible lengths, IPv6 scope, queue overflow/tail loss, write/flush failure, drop without explicit stop.
- [x] Implement fixed magic/version/length header, bounded records with repeated block lengths, mandatory completion footer, reader errors carrying partial-file state; distinguish explicit stop from Drop.
- [x] GREEN: narrow storage tests and format specification. Do not change service.rs, LPD or standard CSV.

### Task 2: Pure synchronization/recovery (root)

**Files:** `crates/langmuir-core/src/lvds_recovery.rs`, `lvds_recovery_tests.rs`, exports in `lib.rs`; minimal shared structure/body validation extraction in `lvds.rs`.

**Interfaces:** `FrameQuality::{Native,Recovered1Bit,Recovered4Bit,RecoveryCandidate,Rejected}`; `RecoveryAnchor { udp_sequence:u32, frame_seq_u8:u8, sample_time_us:u64 }`; `RecoveryContext { udp_sequence:u32, previous:Option<RecoveryAnchor> }`; `LvdsBitAlignmentRecoverer::inspect(payload:&[u8], enabled:bool, context:RecoveryContext)->RecoveryResult`. Result carries actual normalized bytes, optional decoded frame, bit offset/shift, reference CRC input/received/calculated, uniqueness and continuity checks, quality and reason. SHA-256 helper is shared for audit provenance.

- [x] RED: normal altered-CRC frame accepted unchanged and delivered once; unique noisy byte sync succeeds while two valid candidates are rejected.
- [x] RED: independently packed MSB-first offsets 1, 7 and 4 restore full real bits; switch off prevents bit recovery; full fixed header required.
- [x] RED: incomplete 268B/tail bits never produce confirmed science; real 267B prefix can produce diagnostic ADC without invented tail; random noise/ambiguous headers/wrong fixed fields reject.
- [x] RED: full unique recovery requires matching reference CRC and prior reliable same-session u32/seq8/50ms continuity; wraps work, gaps/restarts do not confirm.
- [x] Implement cap of 4096 payload bytes, native fast path, unique bounded byte search, then offsets corresponding to 1-bit/4-bit alignment only. Never join datagrams.
- [x] GREEN: core tests; hash known vector and literal ADC/timestamp invariants.

### Task 3: Backend archival, recovery and bounded history (desktop worker)

**Files:** `apps/desktop/src/backend.rs`, new `backend_history.rs`, `backend_raw_storage.rs`/`backend_recovery_audit.rs` as needed, `bridge.rs`, `mock.rs` exhaustive request handling, new backend tests.

- [x] RED: all actual socket/PCAP UDP (including source mismatch, duplicates, bad envelope and 268B) reaches raw archive before ingress rejection; save state remains independent.
- [x] RED: only native accepted frames enter LPD/11-column CSV; confirmed recovery and optional candidates retain quality/id/provenance in display and audit; source/session/duplicate isolation remains bounded.
- [x] Implement capped recovery envelope acceptance and shared native gate, with persistent asynchronous JSONL audit (source, raw id/hash, outer seq, actual byte dependencies, restored hash, checks/reason).
- [x] RED: opening/pausing/seeking/restarting/exiting each history format, malformed/legacy files, interrupted raw files, and cancellation do not hang or mix sessions.
- [x] Implement background reader with queue 64, sparse index at most 4096 entries (decimate index as input grows), file metadata/progress, requested 60-second window and original frame/time order. No entire-file sample collection.
- [x] RED: scientific CSV export streams complete file frames, ignores plot decimation, cannot overwrite, and includes quality fields in separately versioned recovery exports.
- [x] GREEN: backend/real localhost UDP integration tests; no physical hardware calls.

### Task 4: Integrated desktop controls (root)

**Files:** `app.rs`, new `app_history.rs` / tests if appropriate, existing plot interaction helpers, `main.rs` module declarations and Windows file-picker dependency.

- [x] RED: UI requests for raw start/stop, default-off recovery, quality filters and history controls; status does not claim completeness after raw loss/error.
- [x] Add `打开历史数据` with file selection for all three types, compact independent raw controls/status and recovery switch. Keep fixed columns and chart height; infrequent controls live in a popup/menu.
- [x] Add explicit history mode, file info, play/pause/seek/start/progress/current time+sequence, bounded anomaly view, CSV export and return-live control. Reuse overlay/split, pan/zoom/follow/freeze/reset and min/max decimation.
- [x] GREEN: focused UI behavior/layout tests including automated 1280×720 coverage; normal-size and actual 1362×632 compact native inspection.

### Task 5: Independent data evidence and final delivery

**Files:** `tools/qa/lvds_raw_recovery_reference.py` plus tests (data worker), `docs/LVDS_RAW_CAPTURE_RECOVERY_HISTORY_QA.md`, QA JSON/images and Windows README.

- [x] Independently read actual PCAP and write per-record JSONL (ids, endpoints, outer seq, lengths/prefix, header, bit boundary, candidate offsets, complete/tail evidence, time/seq8, reference CRC range/result, disposition and sample counts). Do not assume 467/237/465.
- [x] Compare all raw archive records byte-for-byte against real PCAP UDP, including metadata and rejected records; run `lvds.txt` normal regression.
- [x] Run `cargo fmt --all -- --check`, `cargo check --workspace --locked`, `cargo test --workspace --features langmuir-desktop/mock --locked`, `cargo build --release -p langmuir-desktop --locked`, strict workspace Clippy and Python tests. Cargo jobs are serialized across workers.
- [x] Build current Windows static-CRT x64 non-Mock release, package only the named EXE/README, compare ZIP bytes and hashes.
- [x] Native Windows GUI: open raw/LPD/PCAP, observe channels, pan/zoom, play/pause/seek/exit; exercise raw recording and recovery controls on user-opened input. Do not connect hardware.
- [x] Report actual PASS/FAIL/BLOCKED/NOT_RUN, limits, bounded-memory/performance evidence, file format compatibility, real-data classifications, package hashes and Git changes. Do not commit/push. Stop within this scope.

### Final display overlap regression and acceptance refresh

Review found that a confirmed recovered frame followed by a Native frame at the same device time appends overlapping samples. `TimeSeriesBuffer` can treat that as clock rollback and clear earlier reliable history. Native scientific storage is already isolated correctly.

- [x] RED: reproduce same-time Recovered-to-Native in live confirmed/diagnostic views and history rebuild, checking prior points remain and Native values take precedence.
- [x] Apply the same display precedence to retained frames/history points without hiding a genuine native clock restart; keep Native LPD/CSV acceptance unchanged.
- [x] GREEN: focused UI tests, desktop suite and strict Clippy; then rerun the final workspace commands and rebuild/package the current Windows executable.
- [x] Open all three formats in the rebuilt native EXE, capture compact layout, repeat SYNTHETIC loopback raw recording, and update the final report with the actual final build evidence.

Final software acceptance evidence is recorded in [the QA report](../../LVDS_RAW_CAPTURE_RECOVERY_HISTORY_QA.md). The frozen workspace run reports 351 passed, 0 failed and 7 ignored; explicit real-input tests separately pass 2/2. The delivered static-CRT EXE SHA-256 is `e8b923dd9818ebc294cc8fdc8bea48235961fd055eb88965dd1c9169fdc6fd1f`. Native GUI validation covers all three history formats, recovery OFF/ON, complete native/quality exports, pan/zoom, playback/pause/seek/EOF/exit, actual 1362×632 compact layout, and a 60-second SYNTHETIC localhost run with 1200 raw records and no drops. Recording/listening were stopped and the software exited normally. Hardware, RS422, hour-scale soak and physical disk-failure GUI tests remain NOT_RUN; no commit or push was performed in this phase.
