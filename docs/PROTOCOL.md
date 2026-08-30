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

## LVDS scientific response (270 bytes)

```text
EB 90 | 55 | AA | 01 07 | AA | UTC seconds | microseconds |
sequence | ch1 first u16 | ch2 first u16 | 248 signed deltas | SUM8
```

The length field is `0x0107` (263 bytes), covering the data type, timestamp, sequence,
and 252-byte data domain; the one-byte checksum is outside that length. The checksum is
the low eight bits of the sum of bytes from the data type through `DATA251`. There is no
LVDS trailer. The documents also mention 269 bytes and a two-byte checksum in prose; the
field table is internally consistent only with this 270-byte interpretation.

The data domain contains 125 samples per channel. `DATA0..DATA3` are unsigned big-endian
16-bit absolute first samples. `DATA4..DATA251` alternate signed two's-complement `i8`
deltas for channels 1 and 2; each sample is reconstructed from the previous sample and
must remain in `0..=65535`. The UTC seconds and microseconds identify the first sample of
both channels. The implementation derives a 400-microsecond sample interval from the
50-millisecond frame period and 125 samples per frame.

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
