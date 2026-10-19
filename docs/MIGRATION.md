# Python-to-Rust migration notes

> **科学数据未经 CRC 完整性验证。** Production LVDS reception, plotting and formal
> saving use structure/time/ADC validation and the existing UDP deduplication gate.
> CRC mode, filtering, details and the separate diagnostic waveform path are removed.

The command and RS422 telemetry protocols follow the supplied 2026-07-24
single-machine specification. They are not wire-compatible with the earlier
Python implementation's 16-byte/8N1 RS422 framing.
The deployed LVDS/UDP protocol uses 269-byte reassembled raw frames, 125 samples per
channel, one timestamp, signed one-byte deltas, and a two-byte big-endian CRC16. The
LVDS body has no packet-sequence field; the UDP wrapper sequence is used as the capture
sequence tag. The UDP reassembler validates the wrapper, lengths, offsets, segment
coverage, duplicates, and conflicting data before outputting a frame. The trailer
bytes remain present and unchanged, but every production decoder now parses time and
ADC without calculating or comparing CRC16. The independent offline reference keeps
polynomial `0x1021`, initial `0xFFFF`, no reflection, no final xor and the historical
`frame[7..267]` (260B) range. That range remains `SPEC_ASSUMPTION`, pending manufacturer
function arguments; it does not govern production acceptance. Captured data with an
unequal trailer can now reach normal plotting and capture if its other checks pass.

Partial UDP assemblies now have a two-second lifetime from first receipt; neither
new nor duplicate fragments renew it. Expiry releases capacity automatically in
desktop and CLI receive loops. Active assemblies remain bounded to 64 slots, with
explicit rejection of new sequences when no slot is free; this adds no cross-frame
ordering, completed-frame deduplication, or storage sequence-format change.

Completed scientific frames pass a separate deduplication gate after structure,
device-time and ADC validation. Within one filtered source/receiver session it retains full `u32`
sequences and raw bytes for five seconds, up to 256 accepted frames, evicting the
oldest accepted record when full. Identical repeats are suppressed; valid different
bytes at a retained sequence are reported as conflicts and withheld. Invalid
science never populates history. Rebinding starts a fresh history; acquisition
start/stop does not. Manufacturer reset semantics remain unspecified, so rollback
alone does not trigger a reset. History expiry/eviction permits later sequence
reuse; this is recent-frame protection rather than permanent exactly-once storage.

Accepted UDP science also feeds an independent full-`u32` sequence quality monitor.
It observes local modular continuity, forward gaps, late fills, disorder and wraps
without changing delivery. Candidates wait two seconds, then retain five seconds
of unresolved expired history, with at most 256 combined records. Huge ambiguous
changes and capacity exhaustion are reported and re-anchor only observation;
they do not prove a reset or network loss. Rebinding creates a fresh monitor, while
acquisition start/stop retains it. The existing metrics/log bridge exposes these
counters; no scientific sequence storage migration or missing-sample generation
is included.

## Operator behavior retained

- independent RS422 and LVDS port/baud selection and lifecycle;
- typed telemetry, frequency, mode, voltage, and LVDS enable/disable commands;
- an explicit raw 12-byte command editing mode for diagnostics;
- telemetry/status display, command and fault logs, CSV export;
- bounded live two-channel plots with overlay/split views;
- start/stop scientific capture and explicit simulation.

There is one production waveform stream and no CRC selection step. A frame accepted
by the structural/time/ADC and UDP deduplication checks is displayed even when capture
is stopped, and goes to the existing batch writer when the user starts saving. The
main counters distinguish matching UDP receipt, parsing success/failure, plotting and
actual sink writes; plotting does not establish persistence.

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
  Separate protocol Mock tests pass artificial CRC golden frames through the real
  UDP reassembler and decoder; desktop tests also invoke the normal controller and
  storage/display path. These are independent of direct GUI sample injection.
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
Native `.lpd` v2 remains a 32-byte header plus 1010 bytes per scientific record. No
schema version, sidecar or CRC quality field is added. Files produced by the current
acceptance policy must be described as lacking CRC integrity verification.
