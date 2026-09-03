# Architecture

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
the request is handled; the physical link stays connected and later frames are ignored
until a new capture is started.

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
the LVDS body has no sequence field. CRC16 uses the lower computer's CCITT-FALSE
parameters; live rejection awaits confirmation of the CRC coverage call site.
