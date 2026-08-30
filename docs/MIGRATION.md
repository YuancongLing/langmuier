# Python-to-Rust migration notes

The wire protocol is unchanged: command, RS422 telemetry, and LVDS scientific
frames use the same bytes and field meanings as `../python`. The Rust decoder is
stricter and rejects bad lengths, fixed fields, checksums, CRCs, invalid microseconds,
truncation, and trailing bytes instead of presenting corrupt data.

## Operator behavior retained

- independent RS422 and LVDS port/baud selection and lifecycle;
- typed telemetry, frequency, mode, voltage, and LVDS enable/disable commands;
- an explicit raw 16-byte command editing mode for diagnostics;
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
- The desktop simulator exercises UI workflows at the Python mock's 100 Hz rate.
  Exact wire decoding, transport fragmentation, and file persistence are exercised
  by the separate headless replay test/benchmark so test code does not enter the
  production backend.
- The Python LVDS decoder silently replaced a channel-1 UTC value outside 2000–2100
  with the host's current time. Rust preserves the received `u32` UTC value and rejects
  only invalid microseconds; it never substitutes host time into captured science data.
- DDS `0` is rendered as the documented **stopped** state instead of being recorded as
  a device fault. DDS `2` and unknown values remain fault conditions.
- Mock waveform values follow a repeatable sequence, while their timestamps intentionally
  use wall-clock time; the desktop simulator as a whole is therefore not bit-deterministic.

## Storage compatibility

HDF5 mode writes `t1_base` (`float64`), `y1_channel` (`int32`, N x 1000), and
`y2_channel` (`int32`, N x 1000), matching the optimized Python logger. The native
`.lpd` layout is documented in `crates/langmuir-storage/README.md`; downstream
Python/HDF5 consumers must either enable HDF5 mode or add an `.lpd` reader.
