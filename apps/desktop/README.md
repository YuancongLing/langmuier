# Langmuier Probe Ground Control Desktop

`langmuir-desktop` is the native `eframe`/`egui` frontend and the production
orchestration layer. Protocol decoding stays in `langmuir-core`, serial I/O in
`langmuir-transport`, and scientific-data persistence in
`langmuir-storage`.

> **科学数据未经 CRC 完整性验证。** Production LVDS decoding checks frame structure,
> device time and both packed code10 channels without calculating or comparing CRC16. Accepted
> UDP frames pass the existing recent-frame deduplication gate, update the single plot
> stream and, while saving is enabled, enter the formal scientific writer. CRC modes,
> filtering, details and diagnostic waveform selection have been removed.

## Run

Production mode is the default. RS422 control uses a real serial port; LVDS
science data is received from an LVDS-to-UDP converter. Scientific saving supports
CSV, portable pure-Rust `.lpd` v3, or both:

```bash
cargo run --release -p langmuir-desktop
```

Optional `hdf5` and `hdf5-static` features retain the legacy 125-point HDF5
implementation. Its schema does not support the current 100-point protocol;
the desktop disables HDF5 selection even when either feature is compiled.
Use CSV or LPD v3 for current scientific captures.

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
- `ui/theme.rs`: the dark palette, spacing, cards, badges,
  LCD-style values, and typography scale.
- `ui/fonts.rs`: validated cross-platform CJK font discovery and fallback.
- `bridge.rs`: typed, non-blocking frontend/backend requests and events.
- `backend.rs`: production controller, serial and UDP reader workers, LPDv3/CSV
  batch writer, and bounded asynchronous telemetry CSV export.
- `buffer.rs`: bounded plot history and min/max-envelope decimation.
- `mock.rs`: feature-gated UI/plot simulator, compiled only by `mock`.

## Desktop presentation

The native layout follows the original Python control center: a narrow RS422
command sidebar, a compact LVDS/UDP acquisition toolbar, a central scientific plot,
a dedicated telemetry/instrument sidebar, and a bounded system log at the
bottom. The dark theme uses slate surfaces, blue primary actions,
green/red status badges, temperature bars, LCD-style values, and 4-by-4 command
byte cards. The window title and header identify the application as
**Langmuier Probe Ground Control**.

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
to scientific capture and plots. Only the final `55/AA` inner addresses are accepted;
`04/FF` is rejected. The 252-byte DATA domain contains 125 packed bytes for each
channel, then `DATA250` (`frame_seq_u8`, frame offset 265) and `DATA251`
(reserved `00`, frame offset 266). Every five packed bytes contain four
consecutive unsigned 10-bit codes, giving 100 codes per channel. The outer UDP
sequence remains a full `u32` for reassembly, deduplication and monitoring;
the stored inner `frame_seq_u8` is independent of it.

The device timestamp belongs to the first sample of both channels. Later sample
times use a 500-microsecond interval, with no extra timestamp shift and no assumed
calendar epoch. The plot shows signed reconstructed ADC values
`code10 * 64 - 32768`, not physical voltage. Raw code10 values remain available
in both scientific storage formats. The trailer bytes
are retained unchanged but not checked for integrity. The core keeps the historical
offline CCITT-FALSE reference (`0x1021`, `0xFFFF`, no reflection, no xorout) over
`frame[7..267]`; this API is not used by production reception, plotting or capture.
UDP, PCAP replay, legacy byte streams and CLI share the same structure/time/code10
decoder. Invalid microseconds, fixed fields or a nonzero reserved byte are rejected,
without inventing device timestamps or repairing wire data.

To inspect a capture, click **离线回放 PCAP**, select its path and start replay. No CRC
setup is needed. To save every frame of a replay, enable **回放时保存科学数据**,
choose a format, then click **开始回放并保存**. Click **开始保存** during
reception/replay to save subsequent Accepted frames. Replay ends by closing the
selected writers; restarting begins at the file header. A replay with no accepted
scientific frames reports its failure reason. Main counters distinguish matching
UDP receipt, parsing success/failure, plotted frames and actual sink writes.

CSV writes 100 paired-channel rows per frame with 11 columns:

```text
frame_index,frame_seconds,frame_microseconds,frame_seq_u8,sample_index,sample_time_us,ch1_code10,ch2_code10,ch1_adc16_reconstructed,ch2_adc16_reconstructed,integrity
```

`sample_time_us` preserves integer device time plus `sample_index * 500`;
`integrity` is `CRC_NOT_CHECKED`. LPD v3 has a 40-byte versioned header and
409-byte records, storing device seconds `u32`, microseconds `u32`, inner seq8,
100 channel-1 `u16` codes and 100 channel-2 `u16` codes, using little-endian
integers. The header identifies the sample count, interval, representation,
sequence width and unchecked CRC/raw device-time flags.

Legacy LPD v2 retains its 32-byte header and 1010-byte records, and legacy HDF5
retains its `[N,125]` schema. Both accept only the isolated
`LegacyScientificFrameV2` model and are not used for new-protocol acquisition.
New frames are not padded or converted to v2. See the
[wire/storage specification](../../docs/PROTOCOL.md) and
[55/AA acceptance report](../../docs/LVDS_55AA_FINAL_ACCEPTANCE.md) for recorded
validation scope. Successful plotting or saving does not establish CRC integrity.

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
spikes. Overlay mode initially uses reconstructed ADC values on a shared scale;
optional **独立归一化** puts each channel's visible range on a normalized scale
and identifies its actual range in the legend. Split mode shows the two
reconstructed-value scales. Display freezing leaves replay and saving running.

DDS status `0` is displayed as **stopped** and logged as a warning on a state
transition, not recorded as a hardware fault. Status `2` remains a fault.
