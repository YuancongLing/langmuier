# Langmuir Desktop

`langmuir-desktop` is the native `eframe`/`egui` frontend and the production
orchestration layer. Protocol decoding stays in `langmuir-core`, serial I/O in
`langmuir-transport`, and integrity-checked scientific-data persistence in
`langmuir-storage`.

## Run

Production mode is the default and enumerates and opens real serial ports. It
stores LVDS captures in the portable pure-Rust `.lpd` binary format:

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
it runs at the Python application's 100 Hz frame cadence even when the UI
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
- `backend.rs`: production controller, serial reader workers, binary/HDF5 batch
  writer, and bounded asynchronous telemetry CSV export.
- `buffer.rs`: bounded plot history and min/max-envelope decimation.
- `mock.rs`: feature-gated UI/plot simulator, compiled only by `mock`.

## Desktop presentation

The native layout follows the original Python control center: a narrow RS422
command sidebar, a compact LVDS/acquisition toolbar, a central scientific plot,
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
use a common 30-pixel control height with explicit vertical centering; labels,
combo boxes, numeric inputs, and checkbox indicators therefore share the same
visual center line.

The UI never parses wire bytes and never writes HDF5 or CSV synchronously.
Scientific capture waits up to 250 ms for bounded storage capacity, so a
momentary full queue does not discard the current frame. A longer stall raises
a fault and stops the capture explicitly; the resulting file is an interrupted
capture rather than being presented as absolutely lossless. Plot updates use a
separate bounded event queue where display-only samples may be dropped.
The critical UI event queue is also bounded; after an extreme UI stall, old
events are counted and summarized instead of consuming unbounded memory. Link
and acquisition state additionally use a coalesced latest-state snapshot, so
the frontend eventually repairs any state transition evicted from that queue.
Transport-event drops and controller-to-UI display drops are reported as
separate per-link counters.

Manual **Stop acquisition** persists the LVDS events already accepted by the
reader event queue when the request reaches the controller. The LVDS link stays
connected, and later frames are intentionally ignored until acquisition is
started again. Disconnect and application shutdown stop the reader first and
drain accepted frames within a bounded stop deadline before closing the capture
writer. If the reader cannot join or its accepted queue cannot drain by that
deadline, the UI raises a fault and writes `<capture>.incomplete` beside the
data file with the timestamp and reason; such a file must not be treated as a
complete capture.

The ten-second plot history keeps an incremental min/max envelope capped at the
display budget. Repainting without new samples reuses that cache instead of
rescanning the million-point raw history, while bucket extrema preserve narrow
spikes. Because `egui_plot` has one Y transform per plot, overlay mode defaults
to independent per-channel normalization and puts each actual range in the
legend; split mode always shows the two original-value scales.

DDS status `0` is displayed as **stopped** and logged as a warning on a state
transition, not recorded as a hardware fault. Status `2` remains a fault.
