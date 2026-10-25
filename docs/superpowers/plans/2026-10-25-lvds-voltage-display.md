# LVDS voltage display implementation plan

**Goal:** Display both LVDS channels and cursor values in volts while retaining original protocol, bridge values and storage bytes.

**Architecture:** Backend events remain reconstructed signed ADC16. Convert once when scientific samples enter desktop display buffers, including native, qualified and history paths. One pure f64 conversion multiplies by 0.00015625; cached values, extrema and cursor lookup are then volts. Keep the generic buffer and all file writers unchanged.

**Tech Stack:** Rust, existing egui/eframe plot buffers and Windows MSVC x64 static CRT release.

**Spec:** User request in this session dated 2026-10-25, seven numerical cases, T1–T7 and native Windows GUI validation.

## Constraints and review focus

- Preserve 55/AA, 269B, two channels of 100 code10, 500us, seq8, CRC policy, UDP and RS422 behavior.
- Preserve raw/LPD v3/11-column scientific CSV and previously generated user files; no Git commit/push or hardware connection during the original acceptance stage. The user subsequently authorized this GitHub publication.
- Convert native/qualified ingress and reliable/diagnostic history points exactly once, including rebuilds and Native precedence.
- Normalized axes are unitless; legends and cursor retain actual volts with two decimals.
- Preserve visible-data auto Y fit and margin, manual XY bounds, freeze, reset, cache capacities and extrema decimation; no forced full voltage range.
- Use actual PCAP and LPD outputs for native screenshots and byte comparison; label any generated negative/zero/positive fixture SYNTHETIC.

## Tasks

- [x] Record source/storage/input baseline hashes and keep existing changes.
- [x] RED: display ingress, legitimate frame/history correspondence, labels/cursor/normalization and manual Y preservation fail before implementation; seven literal numerical/format cases are included in final production tests. Trusted RED: display-red.log.
- [x] GREEN: add desktop display conversion; adapt every display ingress and labels/legend; update old ADC display assertions, preserving raw buffer tests.
- [x] Verify fmt, locked workspace check, workspace Mock tests, strict all-target Clippy; inspect scope and performance/cache regressions. Final result: 357 passed, 7 ignored, 0 failed; all four commands exit 0.
- [x] Independently decode actual PCAP, compare fresh real GUI CSV/LPD byte values, and record actual voltage extrema/sample pairs. New scientific files are byte-identical to the prior ADC-display outputs.
- [x] Rebuild current non-Mock static CRT Windows x64 EXE, preserve previous package, update ZIP and README, verify exact bytes/PE/hash.
- [x] Native GUI: actual PCAP, LPD and LVDSRAW in V; cursor, overlay/split, normalized labels; user TXT-derived PCAP positive/negative waveforms and zero axis; zoom/pan/follow/freeze/reset. Exact zero samples and incoming freeze are covered by automated tests; real hardware/calibration remain NOT_RUN.
- [x] Write docs/LVDS_VOLTAGE_DISPLAY_QA.md with actual evidence and hardware/calibration NOT_RUN.
- [x] Root final package GUI recheck after correcting the test-only history time fixture: actual PCAP full replay/save, new LPD, normalized cursor, TXT positive/negative axis and cursor all verified against original samples. Retain final automated and previous voltage package detailed interaction evidence with explicit package provenance; PE executable code is identical. Final source/protected/backup verification passed 73/12/3; PID 14844 exited, replay/save stopped, no test hardware I/O active. The acceptance stage stopped without Git submission under the then-current instruction; later user authorization permits the present GitHub publication.


## Subsequent publication authorization

After acceptance completed on 2026-10-25, the user explicitly authorized uploading this change to GitHub. Publication preparation only archives byte-identical selected JSON/log/Python evidence under `docs/qa/voltage-display-20261025/` and updates report links. It does not rerun tests/build/GUI or change source, scientific inputs/outputs, or the delivered EXE/ZIP. The original report and this plan were backed up under `target/qa/voltage-20261025/publication-backup/`; the original frozen final manifest remains unchanged. Its report/plan hashes describe that earlier acceptance snapshot. Current document/evidence hashes are recorded separately in `docs/qa/voltage-display-20261025/publication-manifest.json`.
