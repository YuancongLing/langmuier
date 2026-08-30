# Langmuir Probe Control Center (Rust)

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
cd rust
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

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md),
[docs/PROTOCOL.md](docs/PROTOCOL.md), and [docs/TESTING.md](docs/TESTING.md).
Compatibility differences from the Python implementation are recorded in
[docs/MIGRATION.md](docs/MIGRATION.md).
