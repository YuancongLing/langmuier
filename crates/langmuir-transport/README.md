# langmuir-transport

This crate owns physical byte streams and runs protocol decoders on dedicated,
stoppable reader threads. It has no UI or storage dependency.

- `SerialConnector` opens a real serial device when the default `serial`
  feature is enabled. Open/configuration errors are returned; there is no mock
  fallback.
- `ReaderWorker` owns the full-duplex stream, incrementally feeds the matching
  `langmuir-core` decoder, and emits a bounded `ReaderEvent` queue.
- `BackpressurePolicy::Stop` makes the first overflow observable and stops the
  stream, but the overflowing frame (and any later frames already decoded from
  that OS read) cannot be recovered. Lossless capture should use `Wait` with a
  capacity sized for peak bursts and treat a timeout as a capture failure.
  Display-only consumers may explicitly select `DropNewest`.
- Commands use the same I/O owner through a separate bounded queue.
  `try_send_bytes` is non-blocking, `send_bytes_timeout` bounds queue waiting,
  and each returns a `WriteTicket` acknowledging physical `write_all + flush`.
- Reader metrics report receive/transmit counts, decoder rejection and resync,
  event drops, overflows, and queue high-water marks.
- Lifecycle/fault state and per-write `WriteTicket` acknowledgements are
  independent of the best-effort event queue. Shutdown and `Drop` are bounded;
  a transport that ignores its read timeout is detached and counted.

Deterministic packet generators and `MemoryTransport` only exist under tests or
the explicit `mock` feature:

```text
cargo test -p langmuir-transport --features mock
```

The feature-gated headless replay benchmark exercises real 270-byte LVDS
packets through fragmentation, decoding, bounded queues, and `BinarySink`:

```text
cargo run -p langmuir-transport --release --features mock --example replay_bench -- 10000 257
```

Arguments are frame count and fragment size. It reports frames/s, wire MiB/s,
queue high-water marks, drops, overflows, rejected writes, and decoder errors.
