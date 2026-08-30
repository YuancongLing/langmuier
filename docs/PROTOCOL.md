# Wire protocol compatibility

All multi-byte integers use network (big-endian) byte order.

## RS422 telemetry response (64 bytes)

```text
EB 90 | 3A | effective payload (58 B) | SUM8(payload) | 09 D7
```

The effective payload starts with instrument `04` and response type `12`. Parsed fields
are absolute time (`u32`), local counter (`u32`), work mode (`u8`), scan frequency
(`u8`), scan voltage (`u16`), command success/error counters, CPU/PCB temperatures,
three `u16` magnetic-field values, and DDS/DAC/AUX/ADC status bytes. Raw values are not
scaled or signed because the Python source contains no authoritative scaling contract.

## LVDS scientific response (6,026 bytes)

```text
EB 90 | 17 82 | AA | 04 | ch1 UTC/us | ch2 UTC/us |
1000 * (ch1 u24 + ch2 u24) | CRC16 | 09 D7
```

The CRC covers the 6,018 bytes from type through the last sample. It is
CRC-16/CCITT-FALSE (polynomial `0x1021`, initial value `0xFFFF`, no reflection, final XOR
`0x0000`). Samples are unsigned 24-bit values and the interval is 10 microseconds.
UTC seconds are preserved as the received `u32`; microseconds must be below 1,000,000.

## Uplink command (16 bytes)

```text
EB 90 | 0A | 04 | command | data[8] | SUM8(bytes 3..12) | 09 D7
```

Unused data bytes are `9B`.

- `11`: telemetry request.
- `22 55 value`: scan frequency.
- `22 AA value`: work mode.
- `22 BB value_hi value_lo`: scan voltage.
- `33 55`: enable LVDS.
- `33 AA`: disable LVDS.

Unlike the Python decoder, the Rust decoder rejects wrong declared lengths, checksum or
CRC failures, wrong fixed type/device values, invalid microsecond fields, and extra or
truncated bytes. These are correctness fixes, not wire-format changes.
