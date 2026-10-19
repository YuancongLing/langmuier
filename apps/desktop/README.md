# Langmuir Desktop

`langmuir-desktop` is the native `eframe`/`egui` frontend and the production
orchestration layer. Protocol decoding stays in `langmuir-core`, serial I/O in
`langmuir-transport`, and scientific-data persistence in
`langmuir-storage`.

> **科学数据未经 CRC 完整性验证。** Production LVDS decoding checks frame structure,
> device time and both ADC channels without calculating or comparing CRC16. Accepted
> UDP frames pass the existing recent-frame deduplication gate, update the single plot
> stream and, while saving is enabled, enter the formal scientific writer. CRC modes,
> filtering, details and diagnostic waveform selection have been removed.

## Run

Production mode is the default. RS422 control uses a real serial port; LVDS
science data is received from an LVDS-to-UDP converter and stored in the
portable pure-Rust `.lpd` binary format:

```bash
cargo run --release -p langmuir-desktop
```

Choose one HDF5 implementation explicitly when HDF5-compatible output is
required:

```bash
# Use an installed system HDF5 library:
cargo run --release -p langmuir-desktop --features hdf5

# Or build HDF5 from source (requires CMake):
cargo run --release -p langmuir-desktop --features hdf5-static
```

Frontend interaction and plotting simulation is compiled and selected
independently. Its monotonic scheduler catches up every elapsed 10 ms tick, so
it runs at the protocol's 20 Hz frame cadence even when the UI
repaints more slowly, but intentionally bypasses the wire decoder, serial
transport, and persistence
pipeline, so it is not an end-to-end hardware simulation. Both the feature and
the explicit runtime flag are required:

```bash
cargo run --release -p langmuir-desktop --features mock -- --mock
```

Compiling `mock` without `--mock` still starts the production backend. Passing
`--mock` to a binary built without the feature exits without touching hardware.

## Module boundary

- `app.rs`: widgets and presentation state only.
- `ui/theme.rs`: the Python-compatible light palette, spacing, cards, badges,
  LCD-style values, and typography scale.
- `ui/fonts.rs`: validated cross-platform CJK font discovery and fallback.
- `bridge.rs`: typed, non-blocking frontend/backend requests and events.
- `backend.rs`: production controller, serial and UDP reader workers, binary/HDF5
  batch writer, and bounded asynchronous telemetry CSV export.
- `buffer.rs`: bounded plot history and min/max-envelope decimation.
- `mock.rs`: feature-gated UI/plot simulator, compiled only by `mock`.

## Desktop presentation

The native layout follows the original Python control center: a narrow RS422
command sidebar, a compact LVDS/UDP acquisition toolbar, a central scientific plot,
a dedicated telemetry/instrument sidebar, and a bounded system log at the
bottom. The theme uses the same white/slate surfaces, blue primary actions,
green/red status badges, temperature bars, LCD-style values, and 4-by-4 command
byte cards as the PySide6 application.

`egui`'s bundled fonts do not contain Chinese glyphs, so startup validates and
loads an installed CJK face. The lookup covers stable macOS, Windows, and Linux
font locations and validates TTC face indices and all Chinese glyphs currently
used by the desktop sources
before handing bytes to `egui`. Set `LANGMUIR_CJK_FONT` to an explicit
`.ttf`/`.otf`/`.ttc` file when the host uses a non-standard font layout; set
`LANGMUIR_CJK_FONT_INDEX` as well when the desired TTC face is not index zero. If no
valid face can be loaded, the header shows a visible warning instead of silently
hiding the problem.

Proportional UI text uses the selected CJK face first for both Latin and Chinese,
so labels such as `RS422 串口连接` and `LVDS 科学数据` share one baseline and cap
height. Monospace remains limited to pure byte values and timestamps. Form rows
use a common 32-logical-point central control height with explicit vertical centering; labels,
combo boxes, numeric inputs, and checkbox indicators therefore share the same
visual center line.

The current deployed UDP converter sends a 24-byte fragment wrapper plus a 269-byte
LVDS raw frame after reassembly. The desktop toolbar defaults to
`192.168.1.100:1534`, accepts only `192.168.1.128:1534`, preserves UDP datagram
boundaries, and reassembles by frame sequence and byte offset. The frame and rejection
counters confirm live network reception. The reassembled payload is decoded as
`EB 90 | 55 | AA | 01 07 | AA | UTC | microseconds | data[252] | CRC16`, then routed
to scientific capture and plots. The UDP wrapper sequence is used as the stored sequence
tag because the LVDS body does not contain a packet-sequence field. The trailer bytes
are retained unchanged but not checked for integrity. The core keeps the historical
offline CCITT-FALSE reference (`0x1021`, `0xFFFF`, no reflection, no xorout) over
`frame[7..267]`; this API is not used by production reception, plotting or capture.
UDP, PCAP replay, legacy byte streams and CLI share the same structure/time/ADC
decoder. Invalid microseconds or out-of-range reconstructed ADC values are rejected,
without inventing device timestamps or repairing wire data.

To inspect a capture, click **离线回放 PCAP**, select its path and start replay. No CRC
setup is needed. Click **开始保存** during reception/replay to send subsequent Accepted
frames to the existing batch writer. Main counters distinguish matching UDP receipt,
parsing success/failure, plotted frames and actual sink writes. `.lpd` v2 retains its
32-byte header and 1010-byte records, and HDF5 retains its schema. Neither format gains
CRC verification from successful plotting or saving.

The UI never parses wire bytes and never writes HDF5 or CSV synchronously.
Scientific capture waits up to 250 ms for bounded storage capacity, so a
momentary full queue does not discard the current frame. A longer stall raises
a fault, writes a neighboring `.incomplete` marker, and stops the capture
explicitly; the resulting file is an interrupted capture rather than being
presented as absolutely lossless. Storage shutdown is bounded to three seconds
in the desktop backend. Plot updates use a
separate bounded event queue where display-only samples may be dropped.
The critical UI event queue is also bounded; after an extreme UI stall, old
events are counted and summarized instead of consuming unbounded memory. Link
and acquisition state additionally use a coalesced latest-state snapshot, so
the frontend eventually repairs any state transition evicted from that queue.
Transport-event drops and controller-to-UI display drops are reported as
separate per-link counters.

Manual **Stop acquisition** persists the LVDS events already accepted by the
reader event queue when the request reaches the controller. The LVDS link stays
connected, and later Accepted frames continue plotting but are not submitted to the
writer until saving starts again. Disconnect and application shutdown stop the reader first and
drain accepted frames within a bounded stop deadline before closing the capture
writer. If the reader cannot join or its accepted queue cannot drain by that
deadline, the UI raises a fault and writes `<capture>.incomplete` beside the
data file with the timestamp and reason; such a file must not be treated as a
complete capture.

The bounded sixty-second plot history keeps an incremental min/max envelope capped
at the display budget, with a default ten-second visible window. Repainting without
new samples reuses the visible cache, while bucket extrema preserve narrow
spikes. Because `egui_plot` has one Y transform per plot, overlay mode defaults
to independent per-channel normalization and puts each actual range in the
legend; split mode always shows the two original-value scales.

DDS status `0` is displayed as **stopped** and logged as a warning on a state
transition, not recorded as a hardware fault. Status `2` remains a fault.
