# Wire protocol compatibility

All multi-byte integers use network (big-endian) byte order.

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

The deployed UDP wrapper declares every complete LVDS raw frame as `269` bytes. The raw
frame is `EB 90 | 55 | AA | 01 07 | AA | UTC(u32) | microseconds(u32) |
data[252] | CRC16(u16, big-endian)`. `0x0107 = 263` is the declared valid-data length
from `type` through the two-byte CRC16. The LVDS body has no packet-sequence field; the
UDP wrapper's `frame_seq` is retained as the capture sequence tag. The UDP `frame_len`
field defines the actual frame boundary.

CRC16 uses the lower computer's `Calculate_CRC16` implementation: polynomial `0x1021`,
initial value `0xFFFF`, no input/output reflection, and no final xor. The core provides
strict validation over the declared valid-data range. Current captured frames do not
match that range's calculated CRC, so the live receiver parses and retains the CRC field
without rejecting packets until the lower computer's `Calculate_CRC16` call site confirms
its exact `data` pointer and `length`.

The data domain contains 125 samples per channel. `DATA0..DATA3` are unsigned big-endian
16-bit absolute first samples. `DATA4..DATA251` alternate signed two's-complement `i8`
deltas for channels 1 and 2; each sample is reconstructed from the previous sample and
must remain in `0..=65535`. The UTC seconds and microseconds identify the first sample of
both channels. The implementation derives a 400-microsecond sample interval from the
50-millisecond frame period and 125 samples per frame.

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

The Rust decoder rejects wrong declared lengths, wrong fixed type/device values, invalid
microsecond fields, invalid reconstructed samples, and extra or truncated bytes. CRC16
rejection is enabled in the core validation function and will be enabled live once the
lower computer's CRC coverage range is confirmed.
