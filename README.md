# Langmuier Probe Ground Control

This workspace is a Rust rewrite of the application in `../python`. It keeps the wire
formats and the operator workflow while separating protocol processing, hardware I/O,
storage, and presentation.

## Layout

```text
apps/desktop/                 eframe/egui desktop front end
crates/langmuir-core/         protocol codecs, command builder, domain models
crates/langmuir-transport/    serial I/O, acquisition workers, optional mock source
crates/langmuir-storage/      bounded asynchronous batch storage
tools/protocol-cli/           headless protocol inspection and test-data utility
docs/                         protocol, architecture, and verification notes
```

The UI never opens a serial port and never writes a data file directly. It exchanges
typed commands/events with bounded backend queues. Full-rate scientific frames go to a
dedicated storage worker; the UI receives a small loss-tolerant display stream and
decimates it before plotting. A storage timeout is visible and stops capture, so a slow
disk cannot create an unreported gap.

The desktop presentation keeps the Python application's instrument-panel topology and
light aerospace palette. It explicitly installs a validated system CJK fallback because
`egui`'s built-in fonts do not contain Chinese glyphs; non-standard hosts can set
`LANGMUIR_CJK_FONT` to a local font file.

## Build and test

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run --release -p langmuir-desktop
```

No hardware is needed for the test suite. The simulator is isolated behind the `mock`
feature and must be selected explicitly:

```bash
cargo run --release -p langmuir-desktop --features mock -- --mock
```

Failure to open a real port never enables simulation automatically.

LVDS admission checks structure, device time, ADC bounds, and recent duplicates.
CRC16 is not checked for receiving, plotting, or scientific storage: **科学数据未经
CRC 完整性验证**. See [the current acceptance report](docs/LVDS_55AA_FINAL_ACCEPTANCE.md)
for the revised behavior; earlier CRC QA reports describe historical versions.

The desktop also provides independent UDP raw recording (`.lvdsraw`) and opens
LVDSRAW, LPD v3 and PCAPNG histories in the existing dual-channel plot. Optional
bit recovery is off by default and carries explicit quality and provenance;
recovered/candidate data stay out of standard LPD and CSV capture. See the
[desktop workflow](apps/desktop/README.md#raw-recording-and-history) and
[current raw/recovery/history QA](docs/LVDS_RAW_CAPTURE_RECOVERY_HISTORY_QA.md).

For Windows x64 prerequisites, clone/update commands, a non-Mock Release build,
and ZIP packaging, see [docs/WINDOWS_BUILD.md](docs/WINDOWS_BUILD.md).

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md),
[docs/PROTOCOL.md](docs/PROTOCOL.md), and [docs/TESTING.md](docs/TESTING.md).
Compatibility differences from the Python implementation are recorded in
[docs/MIGRATION.md](docs/MIGRATION.md).
