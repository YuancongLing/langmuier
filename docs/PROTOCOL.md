# Wire protocol compatibility

All wire-protocol multi-byte integers use network (big-endian) byte order. The
versioned local storage formats described below use little-endian integers.

> **科学数据未经 CRC 完整性验证。** Production LVDS reception, plots and capture
> do not calculate or compare CRC16. Structure, fixed fields and device time remain
> checked; UDP delivery also passes the existing recent-frame deduplication gate.
> CRC mode, filtering and detail controls have been removed from the desktop.

## RS422 command and telemetry response

```text
EB 90 | sender | receiver | length | type | data[1..4] | SUM8(type+data)
```

RS422 uses `115200` baud, 8 data bits, odd parity, one stop bit (`8O1`), and no hardware
flow control. The telemetry request is a 12-byte frame:

```text
EB 90 F0 55 00 06 11 9B 9B 9B 9B 7D
```

The probe answers only telemetry requests; control commands have no dedicated response.
The 34-byte telemetry response uses sender `55`, receiver `F0`, length `001C`, type `1A`,
and a one-byte additive checksum. It reports request/accepted/rejected counters, times,
mode, frequency, voltage, CPU temperature, AD9833/DAC8563/ADC1/ADC2/LVDS raw status
bytes, and the five-byte last injected command. It does not include PCB temperature,
magnetic field, DDS2, or an auxiliary status field.

## Deployed LVDS scientific response (269 bytes)

```text
UDP fragment wrapper (24 bytes) | LVDS raw frame (269 bytes, after reassembly)
```

The current scientific frame starts with `EB 90 55 AA 01 07 AA` and contains
exactly `269` bytes. Sender `55` and receiver `AA` are required; the earlier
`04/FF` addresses are rejected. `0x0107 = 263` is the declared valid-data length
from `type` through the two-byte CRC16.

| Zero-based frame offset | Length | Field |
| --- | --- | --- |
| 0 | 2 | Header `EB 90` |
| 2 | 1 | Sender `55` |
| 3 | 1 | Receiver `AA` |
| 4 | 2 | Declared length `01 07` |
| 6 | 1 | Scientific type `AA` |
| 7 | 4 | Device seconds, unsigned `u32` |
| 11 | 4 | Device microseconds, unsigned `u32`, less than `1000000` |
| 15 | 125 | `DATA0..DATA124`: channel 1 packed code10 samples |
| 140 | 125 | `DATA125..DATA249`: channel 2 packed code10 samples |
| 265 | 1 | `DATA250`: inner `frame_seq_u8` |
| 266 | 1 | `DATA251`: reserved, must be `00` |
| 267 | 2 | Received CRC16 trailer, not checked in production |

Each channel contains `100` unsigned 10-bit codes in `0..=1023`, packed as four
consecutive codes per five-byte group. Treating each group as one big-endian
40-bit word, the codes occupy bits `39..30`, `29..20`, `19..10` and `9..0`.
This is not the historical absolute-sample/signed-delta representation.

Both channels share the transmitted timestamp of their first sample. Sample
`i` has device time `seconds * 1000000 + microseconds + i * 500` microseconds,
for `i = 0..99`: `500` microseconds per sample, `2000` samples/s per channel,
and a `50` ms frame span. No extra `200` microseconds are added. The device
seconds field is retained without assuming a calendar epoch. The plot uses
signed reconstructed ADC values `code10 * 64 - 32768`, in `-32768..=32704`;
these values are quantization estimates, not a lossless ADC16 recovery or
calibrated voltage.

The UDP `frame_len` field defines the reassembly boundary; the scientific gate
requires 269B. The outer wrapper's `frame_seq` remains a full `u32` used for
reassembly, deduplication and sequence monitoring. It is independent of the inner
`frame_seq_u8` at offset 265 and is not truncated to create the stored sequence.

The two trailer bytes remain in the 269-byte wire layout, but production decoding
does not inspect their CRC value. The retained offline reference API uses the lower
computer's `Calculate_CRC16` algorithm: polynomial `0x1021`, initial value `0xFFFF`,
no reflection and no final xor, over `frame[7..267]` (UTC + microseconds + DATA,
260 bytes). Constants separately define that input range and field offsets 267–268.
The range remains the updated table's working interpretation (`SPEC_ASSUMPTION`),
pending the manufacturer's actual `data` pointer and `length`; it does not affect
production acceptance. Original bytes are never repaired or shifted. A captured
frame can be accepted without satisfying this offline CRC reference, so acceptance
must not be described as proof of CRC integrity or hardware correctness.

## Scientific storage

CSV produces 100 paired-channel rows per accepted frame, in accepted order,
with these 11 columns:

```text
frame_index,frame_seconds,frame_microseconds,frame_seq_u8,sample_index,sample_time_us,ch1_code10,ch2_code10,ch1_adc16_reconstructed,ch2_adc16_reconstructed,integrity
```

The two `code10` columns preserve the received values, reconstructed columns use
`code10 * 64 - 32768`, and `integrity` is always `CRC_NOT_CHECKED`.

New binary captures use `.lpd` v3: a 40-byte header followed by 409-byte records,
with all multi-byte integers little-endian. The header explicitly identifies v3,
100 samples per channel, a 500-microsecond interval, u16 code10 representation,
two channels, an eight-bit inner sequence, raw device time and unchecked CRC.
Each record stores device seconds `u32`, microseconds `u32`, `frame_seq_u8`,
100 channel-1 `u16` codes and 100 channel-2 `u16` codes, without padding.

The historical v2 writer retains its 32-byte header, 1010-byte records, 125 points
per channel and u32 sample representation, accepting only
`LegacyScientificFrameV2`. Legacy HDF5 retains its `[N,125]` schema. Neither
legacy writer accepts the current scientific model; the desktop disables HDF5
for the new protocol. New frames are never padded with 25 points or relabeled
as v2. Older analysis tools must explicitly support v3 to read new LPD files.

See [the final 55/AA protocol acceptance report](LVDS_55AA_FINAL_ACCEPTANCE.md)
for the recorded test inputs, independent readback and validation scope.

## Uplink control command (12 bytes)

```text
EB 90 | F0 | 55 | 00 06 | command | data[4] | SUM8(command+data)
```

Unused data bytes are `9B`; the total frame length is 12 bytes.

- `11 9B 9B 9B 9B`: telemetry request.
- `21 55 frequency 9B 9B`: triangle-wave mode and scan frequency (`frequency=00..64`).
- `21 AA frequency 9B 9B`: sine-wave mode and scan frequency (`frequency=00..64`).
- `21 BB value_hi value_lo 9B`: DC mode and raw scan-voltage setting in big-endian order.
- `31 55 9B 9B 9B`: enable LVDS.
- `31 AA 9B 9B 9B`: disable LVDS.

The `21` and `31` values are the command `TYPE` bytes. The waveform or LVDS
selection (`55`, `AA`, or `BB`) is `DATA1`; the `AA`/`BB` values must not be
promoted to a command type. The source document's table 2-4 uses merged cells
for these type values. The voltage command is encoded as a raw big-endian `u16`;
the source table does not define a separately validated downlink voltage range,
so the telemetry range `0000..1388` is not treated as a hardware-confirmed
downlink limit.

The legacy CLI names `frequency`, `mode`, and `voltage` are rejected with an
explicit migration message; use `triangle`, `sine`, and `dc` for new scripts.
The numeric `mode` command cannot identify the waveform/DC meaning safely.

The Rust decoder rejects wrong declared lengths, wrong fixed addresses/type,
nonzero reserved bytes, invalid microsecond fields, and extra or truncated bytes. Desktop
UDP, PCAP replay, legacy byte streams, and CLI use the same structure → time → code10
decoder. UDP and replay then use the full-`u32` recent-frame deduplication gate;
Accepted frames reach the single plot path and, while capture is enabled, the
CSV/LPDv3 writer. Invalid device time is rejected rather than replaced with host
time. Accepted order and raw timestamps are retained in storage, including valid
device-time rollback; the plot starts a new display window on rollback.
Protocol Mock uses artificial frames through UDP encapsulation/reassembly and the
real decoder; direct GUI sample injection remains a separate UI-only facility.
