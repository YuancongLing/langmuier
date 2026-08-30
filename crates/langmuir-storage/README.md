# langmuir-storage

This crate keeps disk work off the acquisition and UI threads. `BatchWriter`
uses a bounded input queue and never silently discards a scientific frame. A
full queue returns the original value to the caller, which should wait, stop,
or explicitly mark the capture as interrupted instead of hiding a gap.

The default build has no native library dependency. `BinarySink` is the
production default, while `MemorySink` is available only to crate tests or
with the explicit `mock` feature for deterministic integration tests.
Both use explicit constructors; neither is an automatic fallback after a
different sink fails. `BatchWriter` lifecycle/fault status is independent of
its best-effort progress event queue, and shutdown/drop waits are bounded.

## Native binary format (`.lpd`, version 2)

`BinarySink` uses exclusive `create_new` and never overwrites an existing
capture. All integers are unsigned little-endian. The fixed 32-byte header is:

| Offset | Type | Meaning |
|---:|---|---|
| 0 | `[u8; 8]` | magic `LMPROBE\0` |
| 8 | `u16` | format version (`2`) |
| 10 | `u16` | header bytes (`32`) |
| 12 | `u32` | endian marker (`0x01020304`) |
| 16 | `u32` | samples per channel (`125`) |
| 20 | `u32` | sample interval in microseconds (`400`) |
| 24 | `u32` | bytes per frame (`1010`) |
| 28 | `u32` | flags (`1`: UTC seconds + microseconds + sequence) |

Each fixed 1010-byte frame contains, in order: UTC seconds (`u32`), microseconds
(`u32`), packet sequence (`u16`), 125 channel-1 samples (`u32`), and 125 channel-2
samples (`u32`). A file is valid up to its last complete frame; an incomplete
trailing frame indicates an interrupted/failed write.

## Optional HDF5 output

Production HDF5 output is explicit:

```text
cargo build -p langmuir-storage --features hdf5
```

That feature uses `hdf5-metno` and the HDF5 installed on the system. Use
`hdf5-static` to build HDF5 and zlib from source (CMake and a C compiler are required).
Both modes write `t1_base` (`float64`), `sequence` (`uint16`), `y1_channel`
(`int32`, N x 125), and `y2_channel` (`int32`, N x 125).
The file attribute `committed_frames` records the last batch accepted by all
HDF5 write calls; failed batches are truncated back to that count on a
best-effort basis. This supports process-local error recovery but is not an
atomic power-loss transaction, because HDF5 may still cache both data and the
attribute until flush/close.

Mock transports are not selected here and a serial/open failure elsewhere must
never fall back to a sink or simulated source automatically.
