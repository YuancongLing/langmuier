# Architecture

> **科学数据未经 CRC 完整性验证。** Production LVDS acceptance checks structure,
> device time and ADC ranges, then the existing UDP deduplication gate. It does not
> calculate or compare CRC16 before plotting or formal capture. The desktop has one
> plot path and no CRC mode, filter or detail controls.

## Boundaries

The application is split along ownership boundaries rather than UI screens:

```text
operator -> desktop UI -> bounded control channel -> acquisition backend
                                                    |-> RS422/LVDS transport
                                                    |-> protocol core
                                                    `-> bounded storage channel
                                                         |-> native .lpd (default)
                                                         `-> HDF5 (explicit feature)

backend -> bounded critical/display channels -> UI state -> decimated plots
```

- `langmuir-core` is deterministic and contains no UI, serial, filesystem, or mock
  dependency. It validates frames and owns the typed domain model.
- `langmuir-transport` owns every serial handle and its worker lifecycle. A damaged
  candidate advances one byte before resynchronization so an embedded valid header is
  not discarded.
- `langmuir-storage` owns native-binary or HDF5 handles on one writer thread. Shutdown
  drains queued frames, writes the final partial batch, flushes, and closes the sink.
- `langmuir-desktop` renders state and emits intents. It does not parse wire bytes or
  block on disk I/O.
- mock generation is compiled only for tests or the explicit `mock` feature.

## Throughput and backpressure

Each decoded scientific frame owns two boxed 125-sample arrays and moves into the
storage queue without a sample-array clone. A floating-point display copy is allocated
only when the small display queue has room. Per-sample timestamp arrays are never stored:
timestamps are reconstructed from the base time and fixed 400 microsecond interval.

Storage is the loss-sensitive path. Submission waits for a bounded interval; if storage
cannot keep up, the failure is surfaced and capture is stopped instead of silently
continuing with a gap. UI delivery is loss-tolerant: stale scientific display events may
be dropped, and the operator sees the corresponding counters. Both UI event queues and
plot histories have fixed capacities. The UI plots a bounded min/max envelope rather
than all received points; repainting without new data reuses an incremental cache.

## Lifecycle

Connection and acquisition are explicit states. Shutdown order is:

1. reject new start/send requests;
2. ask the LVDS reader to stop, drain all frames it already accepted, and join or report
   a bounded shutdown timeout;
3. drain the storage queue, write the final partial batch, and close the capture;
4. stop the RS422 reader and the CSV export task;
5. report any lifecycle error without allowing a non-cooperative driver to hang UI exit.

If the producer cannot join, its accepted-event queue cannot drain within the bounded
deadline, or storage fails, the capture is finalized when possible and a neighboring
`.incomplete` marker is written with the timestamp and reason. The UI never reports
that path as a complete drain. Desktop storage shutdown is bounded to three seconds.

The manual **Stop acquisition** boundary includes the LVDS events already present when
the request is handled. The physical link stays connected and later Accepted frames
continue to update plots; they are not submitted to storage until capture starts again.

There is no automatic real-device-to-mock fallback.

## Physical-link caveat

At 20 frames/s, a 269-byte LVDS raw frame is 5,380 bytes/s. RS422 command/telemetry
uses 115200 baud with odd parity (`8O1`). Scientific data arrives through an
LVDS-to-UDP converter. Each deployed UDP datagram has a 24-byte fragment header and
carries a portion of one 269-byte LVDS frame; the receiver preserves UDP datagram
boundaries and reassembles by the header's byte offset and frame sequence. A one-fragment
frame is therefore 293 bytes on the UDP payload (`24 + 269`).

Both `langmuir-protocol-cli receive-udp-lvds` and the desktop backend preserve UDP
datagram boundaries and reassemble the deployed wrapper. The desktop backend binds
`192.168.1.100:1534` by default and filters `192.168.1.128:1534`; it exposes live
reassembly and rejection counters, decodes the reassembled 269-byte frame, and sends its
125-point channels to storage and plots. The UDP wrapper sequence tags each stored frame;
the LVDS body has no sequence field. The desktop still truncates the UDP `u32` tag to
`u16`, pending a separate reliability/storage revision. All production entry points
check structure, time and ADC bounds without calculating or comparing CRC16; a
decodable frame can be plotted and formally captured regardless of its trailer value.
The two trailer bytes remain unchanged in the wire format. The core retains an
independent offline CCITT-FALSE reference API over `frame[7..267]` (260B), using
`poly=0x1021`, `init=0xFFFF`, no reflection and `xorout=0`. That historical range remains
`SPEC_ASSUMPTION` and is not a production acceptance condition.

Incomplete UDP assemblies expire two seconds after their first fragment, using
`Instant`; duplicates do not renew their lifetime. Cleanup runs before fragment
admission and on the desktop receiver's idle polls. With all 64 live slots occupied,
new sequences are explicitly rejected until a slot completes or expires. Reassembly
timeouts, capacity rejections, fragment errors, and scientific parsing failures have
separate counters; existing 500ms diagnostics aggregate anomalies. Historical CRC
mismatches do not create production rejection counts or connection failures.

After structure/time/ADC validation, a source/session-scoped `UdpLvdsScienceGate`
compares the complete UDP `u32` sequence and all 269 raw bytes. It suppresses
identical repeats and separately rejects valid same-sequence content conflicts
before the normal controller or CLI capture writes. Its independent five-second,
256-entry FIFO history is bounded and never renewed by duplicates or conflicts.
Rebinding creates a fresh session; sequence rollback alone does not clear history.
This adds no cross-frame ordering or storage-format change.

Accepted science always feeds the bounded display path and feeds `BatchWriter` only
while capture is enabled. Native `.lpd` v2 retains its 32-byte header and 1010-byte
records; HDF5 retains its schema. Those formats do not add CRC quality flags, and
successful enqueue, sink writes and plots remain distinct from file close/flush.

Only Accepted frames then update a separate source/session-scoped
`UdpLvdsSequenceMonitor`, using the untruncated outer `u32` before the original
callback or CLI write. Local modular advances reveal unobserved valid-science
candidates; they wait two seconds and retain unresolved expired history for five
more seconds. Waiting and expired records together are bounded to 256. Late fills
resolve records without moving the frontier backward. Large ambiguous changes or
capacity pressure re-anchor observation and explicitly account for forgotten
records, without inferring a device restart. Wraps, disorder, deadline events and
ambiguities flow through existing snapshots and aggregate diagnostics. Expiry is
not proof of UDP network loss. Monitoring neither reorders data nor inserts samples;
the original TTL, deduplication and storage formats remain independent.
