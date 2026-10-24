# LVDS 55/AA Final Protocol Implementation Plan

> **For agentic workers:** Execute the authorized correction in this session with parallel file ownership and independent final review. No Git commits.

**Goal:** Strictly accept final `EB 90 55 AA 01 07 AA` scientific frames and verify native Windows plotting plus CSV/LPD v3 against the supplied 20 original frames.

**Architecture:** Change the shared core address constants, preserve the existing decoder and ScienceGate, and update semantic fixtures and wrong-address diagnostics. Read the user's original TXT directly, derive an independent Python reference and a byte-preserving test-only PCAPNG wrapper, then compare Rust decoder and native GUI files against that reference.

**Tech Stack:** Rust workspace, egui native Windows desktop, MSVC static CRT Release, Python independent readback.

**Spec:** Latest instructions `C:/Users/Administrator/.codex/attachments/f2665c3b-8c21-4dae-b201-b9f5189f9f8e/Pasted text.txt` supersede the earlier external-test-package requirement. User subsequently supplied the original 20 HEX lines directly in chat. These were transferred unchanged into root `lvds.txt` using ASCII/LF with a final newline; never synthesize replacement payloads.

## Global Constraints

- Only accept SRC `0x55`, DST `0xAA`; reject `0x04/0xFF`, no version guessing.
- Preserve 269B, 100 code10 samples/channel, 500us, original first sample time, seq8, reserved `00`, unchecked CRC, existing CSV and LPD v3 layout.
- Preserve RS422, outer UDP 24B, TTL, dedup, P1-3, plots and storage lifecycle.
- Preserve all existing work and original input files; do not add, commit or push Git, connect hardware, send commands, or add a simulation mode.

## Review Focus

- Final address positive and both individual address errors: decoder and stream tests.
- Obsolete wrong-address EOF hint must identify `04/FF`, including mixed captures: desktop replay tests.
- Historical `55/AA` PCAP may pass address but fail other fields: independently audit without assuming payload compatibility.
- Native dual save must start before the first frame and flush at EOF: GUI state and full independent CSV/LPD readback.
- Original HEX and constructed PCAP must match byte for byte: independent input audit, hashes and source labels.

### Task 1: Shared decoder and strict address contract

**Files:** `crates/langmuir-core/src/lvds.rs`, `lvds_protocol_tests.rs`, `lvds_test_support.rs`; review `framing.rs`.

**Interfaces:** `decode_scientific_frame(&[u8]) -> Result<ScientificFrame, FrameError>` and existing stream decoder remain unchanged.

- [x] Write literal `55/AA` acceptance and `04/FF` rejection assertions; run them against the current decoder and observe RED.
- [x] Correct only `LVDS_SENDER_ADDRESS`/`LVDS_RECEIVER_ADDRESS`; adapt positive fixtures and prior rejection assertions by semantic role.
- [x] Run core tests, including streaming, golden vectors, time, CRC independence, dedup and sequence quality.

### Task 2: Desktop and remaining fixture consistency

**Files:** `apps/desktop/src/backend.rs`, `backend_replay_tests.rs`, `app.rs`, desktop/transport/CLI fixture files, `tools/qa/lvds_10bit_reference.py`.

**Interfaces:** Keep replay progress, gate, application state and storage formats unchanged.

- [x] Update wrong-address replay expectations first; observe failing diagnostics before updating production diagnostic counting and copy.
- [x] Replace only scientific header address literals in positive fixtures; retain payload `04`/`FF`, RS422 and UDP magic.
- [x] Keep both wrong-address rejection and mixed-capture behavior tests; update current UI protocol copy.
- [x] Run focused desktop/CLI/transport tests, then fmt/check/full tests and strict Clippy in default and Mock configurations.

### Task 3: Original data audit and independent reference

**Files:** User input files remain read only after the explicit chat-to-file transfer; `tools/qa/lvds_txt_to_pcapng.py` and unit tests, `tools/qa/lvds_txt_reference.py` and unit tests; acceptance tests in `apps/desktop/src/backend_10bit_tests.rs`. Current user-data evidence under `target/qa/lvds-55aa-user` and `docs/qa`.

- [x] Read the latest user-supplied 20 chat HEX lines, transfer unchanged into root `lvds.txt`, and record the resulting file SHA256. ASCII/LF plus final newline: 10780B, `b5a2eee986fbe4f26da33da67bfeab17b8f9185f72ed0c6f81f232f3c2f15add`. This is the transferred artifact hash, not an unknown pre-existing disk file hash.
- [x] Test-first strict HEX parser: line-number errors for blank/nonhex/odd/truncated/oversized lines, trim only ends; preserve order and bytes. Independent wrapper/timing/endpoint/no-overwrite tests pass. Reuse existing PCAPNG block builder. Real PcapNgReader interoperability passed for clearly labeled UNIT frames and the user20 derived PCAP.
- [x] Python standard-library reference independently unpacks each 40bit group at shifts30/20/10/0; generates 11-column expected CSV and verifies all native CSV/LPD fields with exact line/record/sample mismatch details. Malformed input, every CSV column, LPD header/record/EOF mutation, and real CLI path tested: 28 Python tests pass.
- [x] Independently confirm 20x269B, header, seq0..19, seconds1700000000, micros+50000, both 2000-point channels, 500us continuity and first golden values. Python supplied-baseline PASS; generated 2000-row reference CSV and full manifest.
- [x] Generate the user TXT-derived PCAPNG without changing inner bytes: `target/qa/lvds-55aa-user/lvds-from-txt-derived.pcapng`, 7516B, SHA256 `e082a83fd079df35484f42b891b59e294849b747874be7410aedc8fee5c42813`; provenance explicitly identifies artificial network wrapping.
- [x] Confirm locally derived PCAP inner frames equal TXT, provenance says original HEX plus artificial UDP/PCAPNG wrapper. Actual PcapNgReader verified20 packets, endpoints, full outer u32,50ms, all inner bytes and EOF. Independent complete chat-literal audit also confirms all5380 decoded bytes including unchanged CRC tails. Input SHA256 unchanged.
- [x] Use current Rust production decode and BinaryV3Reader to check complete samples, time and sequence; retain reference script stdout and exit codes. Both explicit user-data tests PASS; independent Python compares all4000 codes and all11 CSV columns.

Two explicit ignored user-data acceptance tests in `backend_10bit_tests.rs` now pass when supplied the actual TXT/reference/derived PCAP/native files. Each command enables `mock` feature for the existing Rust test environment, but exercises production decoder/Ingress/Controller and actual readers; the native application itself is non-Mock. Logs: `target/qa/lvds-55aa-user/rust-original-positive-negative.log` and `rust-derived-pcap-native-readback.log`, each1 passed/0 failed/exit0. Default regression still reports4 ignored.

### Task 4: Native Windows delivery and final report

**Files:** `docs/WINDOWS_BUILD.md`, `docs/LVDS_55AA_FINAL_ACCEPTANCE.md` (latest required name), historical report pointers, current QA artifacts and delivery outputs.

- [x] Rebuild fresh x64 non-Mock Release with static CRT and no HDF5; document exact command/result. Initial final-address build37.68s; rerun after test additions succeeded and yielded the same EXE hash as the native synthetic smoke.
- [x] Launch with a writable working directory, choose LPD + CSV, replay positive with writers before first frame, observe both curves, EOF and closed writers. Actual delivery non-Mock EXE:20 received/parsed/accepted/plotted/saved,0 failures; enqueue/write20,CSV2000. Tested wheel zoom,left pan,reset/follow,freeze/resume,overlay/split. Four actual screenshots under `docs/qa/LVDS_55AA_USER_*_WINDOWS.jpg`.
- [x] Compare all 11 CSV columns against TXT-derived2000-row reference; verify8220B LPD, v3 header and all20 records/4000codes with independent Python and Rust reader. GUI stem `scientific-20261024-181541.169`, full evidence `docs/qa/LVDS_55AA_USER_NATIVE_READBACK.json`. CSV SHA256307b5501...,LPD7cab7380...; normalEOF,500us,seq0..19.
- [x] In-memory04FF negative from each TXT frame changes only offsets2/3:20 fixed-address rejects/0accepted/0plot/0enqueued/0records, source file unchanged.
- [x] Update README exact final protocol line and TXT conversion/readback steps, rerun non-Mock Release build, regenerate ZIP and verify bytes, PE/imports, ZIP content/CRC and SHA256. Current EXE unchanged from the native synthetic smoke.
- [x] Independent scope/cross review, historical original PCAP hashes and diff whitespace check; finalize `docs/LVDS_55AA_FINAL_ACCEPTANCE.md` plus machine evidence. No commits. Regression:Rust287/0/4ignored,Python28/0,fmt/check/strictClippyPASS. Additional explicit user20 tests each1 passed/0failed; actual GUI/Python/Rust reader acceptance PASS.

**Final status:** PASS for the user-supplied20 HEX frames under final55/AA. ASCII/LF chat transfer and complete-byte audit, Python/Rust4000-code comparison,20-frame positive and address-negative gates, real-reader byte preservation, actual non-Mock Windows GUI,CSV2000 and LPD8220 full readback all pass. Input hash remainsb5a2eee9...; EXE9f3035c9... and current ZIPbb3f8cef... unchanged. Only0.9995s sample span is evidenced; synthetic200 remains separate, real probe communication NOT_RUN. Stop after this task; no commits or new phase.
