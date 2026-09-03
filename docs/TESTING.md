# Verification without hardware

The normal test suite is deterministic and opens no real device.

Coverage includes:

- golden RS422, LVDS, and command vectors;
- every decoded field and big-endian boundary;
- checksum, header, declared-length, type, and address failures;
- fragmented input at arbitrary byte boundaries, concatenated frames, leading noise,
  corrupt frames, and byte-wise stream input;
- UDP/LVDS wrapper validation, source filtering, and reassembly of deployed 269-byte
  raw frames before the desktop backend reports live reception;
- bounded-channel behavior and clean worker shutdown;
- native storage headers/records, partial and full batches, final flush, queue timeout,
  sink failure, and bounded shutdown;
- a headless exact-wire pipeline test from fragmented `MemoryTransport` input through
  the real LVDS decoder and reader worker to `BinarySink`, including `.lpd` readback;
- Python-compatible HDF5 dataset names, shapes, and values when an HDF5 build
  dependency is available and the feature is selected;
- feature-gated 20 Hz desktop UI simulation when both the `mock` feature and
  `--mock` flag are selected (this UI simulator is not the wire-pipeline test);
- CJK fallback selection, TTC face validation, representative Chinese glyph
  coverage in both font families, and Python-palette theme contracts;
- desktop and command-line target compilation.

Useful commands:

```bash
cargo test --workspace
cargo test --workspace --features mock
cargo clippy --workspace --all-targets --features mock -- -D warnings
cargo build --release -p langmuir-desktop
```

HDF5 is intentionally checked separately because it requires either system HDF5 headers
or CMake for `hdf5-static`:

```bash
cargo test -p langmuir-storage --features hdf5
# or
cargo test -p langmuir-storage --features hdf5-static
```

For a repeatable mock throughput measurement using complete 269-byte test packets,
fragmentation, both bounded worker queues, and real `.lpd` writes:

```bash
cargo run -p langmuir-transport --release --features mock \
  --example replay_bench -- 10000 257
```

The two arguments are frame count and fragment bytes. The tool reports frames/s, wire
MiB/s, queue high-water marks, drops, overflows, rejected storage items, and decoder
errors. It deliberately has no hard-coded timing assertion: the target rate and test
machine must be stated when accepting performance. Hardware tests, when added, must
remain `#[ignore]` and require an explicit device path.

## Last local verification

On 2026-08-05, the default workspace passed 72 tests and the `mock` workspace
passed 73, strict workspace Clippy completed with warnings denied in both modes,
formatting passed, and the release desktop binary built successfully. The macOS
Mock application was also visually inspected at 1,360 × 860 with disconnected
and active telemetry states; Chinese labels, the five-region layout, equal-width
command cards, plots, telemetry instruments, and logs rendered correctly. A
follow-up visual pass also verified mixed Chinese/English label baselines and the
common center line for labels, combo boxes, numeric inputs, and checkboxes. A 10,000-frame replay
with 257-byte fragments completed at 47,180.5 frames/s (271.14 MiB/s of wire
data) with reader queue high-water 8, storage queue high-water 29, and zero
drops, overflows, storage rejections, checksum failures, or length failures on
this machine.

This is a synthetic memory-transport and temporary-file result, not a hardware
guarantee. Other operating systems and display scales, the real serial/LVDS adapter
and driver, sustained target-machine disk performance, and cable/device timing still
require target acceptance. Optional HDF5 tests could not run on this machine because system HDF5
headers and CMake were not installed; the default `.lpd` path does not require them.
