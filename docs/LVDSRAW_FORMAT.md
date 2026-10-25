# LVDSRAW version 1

`.lvdsraw` stores exactly the UDP payload bytes obtained by the application,
before source filtering, envelope parsing, reassembly, deduplication or science
decoding. It contains no invented Ethernet/IP headers. LIVE socket observations
do not establish whether packets were lost before delivery to the application.
Scientific LPD v3 and the standard 11-column CSV remain separate formats.

All integers below are unsigned and little endian. Timestamps are Unix seconds
(`u64`) and nanoseconds (`u32`, less than 1,000,000,000), preserving `Duration`
without converting it to an overflowing `u64` count of nanoseconds. File order
is arrival order; timestamps are never sorted or repaired.

## Header: 128 bytes

| Offset | Size | Meaning |
|---|---:|---|
| 0 | 8 | ASCII `LVDSRAW` followed by NUL |
| 8 | 2 | Version = 1 |
| 10 | 2 | Header length = 128 |
| 12 | 4 | Endian marker = `0x01020304` |
| 16 | 4 | Origin: 1 = LIVE_UDP, 2 = PCAP_REPLAY |
| 20 | 4 | Maximum UDP payload = 65,535 bytes |
| 24 | 8 | Creation timestamp seconds |
| 32 | 4 | Creation timestamp nanoseconds |
| 36 | 4 | Reserved flags = 0 |
| 40 | 16 | Capture/session identifier supplied by caller |
| 56 | 24 | Configured expected source endpoint |
| 80 | 24 | Configured local listener binding |
| 104 | 24 | Reserved = 0 |

An endpoint occupies 24 bytes: family `u8` at 0 (4 or 6), reserved zero byte at
1, port `u16` at 2, address bytes at 4–19, and IPv6 scope ID `u32` at 20. IPv4
uses the first four address bytes and zeroes the remaining twelve and scope ID.
IPv6 uses the network-order 16 address bytes. IPv6 flow information is not part
of this endpoint schema; nonzero flow information is rejected by the writer.
An unknown destination uses 24 zero bytes (family 0); other endpoints cannot be
unknown. A binding such as `0.0.0.0:1534` identifies the socket configuration,
not the actual destination address of a received datagram.

## DATA block: 100 + payload length bytes

| Offset | Size | Meaning |
|---|---:|---|
| 0 | 2 | Block kind = 1 |
| 2 | 2 | Fixed record header length = 96 |
| 4 | 4 | Total block length = 100 + payload length |
| 8 | 8 | Original record ID, starting at 1 |
| 16 | 8 | Observation/capture timestamp seconds |
| 24 | 4 | Observation/capture timestamp nanoseconds |
| 28 | 4 | Record flags described below |
| 32 | 24 | Source endpoint |
| 56 | 24 | Destination endpoint, or all zeroes when unknown |
| 80 | 4 | Payload length, 0–65,535 |
| 84 | 4 | Reserved = 0 |
| 88 | 8 | Original packet index, or zero when absent |
| 96 | N | Exactly N original UDP payload bytes |
| 96 + N | 4 | Repeated total block length |

Flags: bit 0 is `source_matches`; bit 1 indicates presence of the original
packet index. Bits 2–3 encode destination semantics: 0 = unknown, 1 = actual
captured destination, 2 = socket binding; 3 is invalid. Remaining bits are zero.
The source-match flag must equal comparison with the header's expected source.
The original packet index can include non-UDP packets in a PCAP and need not
equal the raw record ID. Its absent representation is zero with bit 1 clear.

IDs are assigned before attempting the bounded queue submission. They are
strictly increasing, so records rejected by a full queue leave gaps. Duplicate,
wrong-source, malformed, short and misaligned UDP payloads remain distinct raw
records. No science CRC, frame-header, time or envelope validation changes the
stored payload bytes. Zero-byte UDP payloads are valid raw observations.

## END block: 92 bytes

| Offset | Size | Meaning |
|---|---:|---|
| 0 | 2 | Block kind = 2 |
| 2 | 2 | Footer header length = 88 |
| 4 | 4 | Total block length = 92 |
| 8 | 8 | Total observations while recording |
| 16 | 8 | Successfully enqueued records |
| 24 | 8 | Records confirmed written and flushed |
| 32 | 8 | Dropped observations |
| 40 | 8 | Confirmed payload bytes |
| 48 | 8 | Last observed record ID, zero for an empty recording |
| 56 | 4 | State: 1 = complete, 2 = incomplete |
| 60 | 4 | Stop reason: 1 = explicit stop, 2 = abandoned |
| 64 | 8 | Last written record ID, zero when no records were written |
| 72 | 16 | Reserved = 0 |
| 88 | 4 | Repeated total block length = 92 |

There is exactly one END block and no trailing bytes. The reader cross-checks
its record count, payload-byte total, last written ID and all counter
relationships. For an orderly drained recording, observed = enqueued + dropped,
enqueued = written, and last observed ID = observed. Complete additionally
requires explicit stop and zero dropped observations. An incomplete END can
describe a successfully closed file that contains a known queue overflow.

The mandatory END detects interruption at an otherwise valid record boundary.
EOF without END is incomplete; a partial header, record, payload, repeated
length or footer is truncated. Neither condition is a successful normal EOF.
Complete preceding records remain readable for diagnostic/history use. Unknown
versions, flags, block kinds, invalid endpoints and inconsistent lengths are
rejected. Lengths are checked before payload allocation. This v1 container has
no content checksum: its lengths and END detect structural truncation and known
application drops, not arbitrary bit corruption. Recovery audits calculate
their own SHA-256 values from the preserved original payload bytes.

## Writer lifecycle and memory bound

`RawArchive` owns its own `BatchWriter<RawRecord>` and is independent of science
saving. Defaults are queue capacity 128, batch size 32, a 250 ms flush interval,
and a 64 KiB file buffer. The maximum payload memory for queue, active batch and
one pending observation is `(128 + 32 + 1) * 65,535` bytes, plus record metadata
and the file buffer. Every payload is limited to 65,535 bytes.

Submission is nonblocking. Queue overflow drops the new archive item, increments
the reliable dropped count and permanently prevents a complete state, while the
caller can continue science processing. File creation uses `create_new`; an
existing path is never overwritten. Batches count only after `flush` succeeds.
Write or flush failure prevents further writes and cannot produce a complete
END. Failed buffers are discarded without an implicit retry during Drop.

Only explicit `RawArchive::stop` requests a normal END. Dropping an active
archive marks it abandoned before the bounded worker's cleanup runs. Process
termination may leave no END at all. Read/write/close errors and queue losses
are kept in shared accounting, separately from the bounded progress-event
queue. Ordinary flushing does not promise persistence through power loss.
