# Python-to-Rust migration notes

The command and RS422 telemetry protocols follow the supplied 2026-07-24
single-machine specification. They are not wire-compatible with the earlier
Python implementation's 16-byte/8N1 RS422 framing.
The deployed LVDS/UDP protocol uses 269-byte reassembled raw frames, 125 samples per
channel, one timestamp, signed one-byte deltas, and a two-byte big-endian CRC16. The
LVDS body has no packet-sequence field; the UDP wrapper sequence is used as the capture
sequence tag. The UDP reassembler validates the wrapper, lengths, offsets, segment
coverage, duplicates, and conflicting data before outputting a frame. CRC16 uses
polynomial `0x1021`, initial `0xFFFF`, no reflection, and no final xor. Captured frames
do not match the documented coverage range, so strict CRC rejection awaits confirmation
of the lower computer call site's `data` pointer and `length`.

## Operator behavior retained

- independent RS422 and LVDS port/baud selection and lifecycle;
- typed telemetry, frequency, mode, voltage, and LVDS enable/disable commands;
- an explicit raw 12-byte command editing mode for diagnostics;
- telemetry/status display, command and fault logs, CSV export;
- bounded live two-channel plots with overlay/split views;
- start/stop scientific capture and explicit simulation.

## Deliberate differences

- Real serial receive and physical command-write acknowledgements are wired in
  the production backend. The Python code had incomplete/commented real-I/O paths.
- The default capture format is the pure-Rust `.lpd` format, avoiding a native
  HDF5 runtime dependency. Python-compatible HDF5 dataset output remains available
  through the explicit `hdf5` or `hdf5-static` feature.
- The display path may drop stale plot updates, while the storage path has its own
  bounded queue and stops capture on a reported timeout. This prevents UI rendering
  from silently controlling scientific-data integrity.
- The simulator requires both the compile-time `mock` feature and the `--mock`
  runtime flag. A hardware error never falls back to simulation.
- The desktop simulator exercises UI workflows at the protocol's 20 Hz rate.
  Exact wire decoding, transport fragmentation, and file persistence are exercised
  by the separate headless replay test/benchmark so test code does not enter the
  production backend.
- The Python LVDS decoder silently replaced a channel-1 UTC value outside 2000–2100
  with the host's current time. Rust preserves the received `u32` UTC value and rejects
  only invalid microseconds; it never substitutes host time into captured science data.
- The current RS422 telemetry response defines raw status bytes for AD9833, DAC8563,
  ADC1, ADC2, and LVDS, but does not define numerical normal/fault semantics. The UI
  displays the received hexadecimal values and does not infer a fault from them.
- Mock waveform values follow a repeatable sequence, while their timestamps intentionally
  use wall-clock time; the desktop simulator as a whole is therefore not bit-deterministic.

## Storage compatibility

HDF5 mode writes `t1_base` (`float64`), `sequence` (`uint16`), `y1_channel`
(`int32`, N x 125), and `y2_channel` (`int32`, N x 125). The native
`.lpd` layout is documented in `crates/langmuir-storage/README.md`; downstream
Python/HDF5 consumers must either enable HDF5 mode or add an `.lpd` reader.
