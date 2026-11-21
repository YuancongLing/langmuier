# QA data included in Git

The repository includes small protocol golden vectors, synthetic PCAPNG fixtures,
an original-byte real capture subset, and historical verification summaries/logs.
The [real multimode fixture](../../tests/fixtures/lvds-multimode/README.md) is the
portable sample for trying AA/AB/AC history and all 11 AB configurations.

Full original captures, expanded per-sample `independent-manifest.json` files
(about 7.5 MB and 22.3 MB), saved CSV/LPD/LVDSRAW recordings, screenshots,
build directories, EXEs, and packaged archives are local artifacts. Historical
acceptance reports retain their original paths, hashes, process identities and
scope; links to these local-only artifacts will not resolve in a fresh clone.
They are not required for the default test suite. Tests needing full original
captures remain explicitly ignored and must not be run using the small subset
as a substitute for a complete-capture acceptance.

The source snapshot and machine identity JSON files describe the original local
acceptance run. They do not assert that the same absolute paths or running
processes exist on another computer. Native Windows GUI and real hardware
limitations remain exactly as stated in those reports.

## Regenerate a complete-capture reference

With the original `202611212141.pcapng` and the included protocol workbook,
create a new output directory under `target/qa`:

```powershell
python tools/qa/lvds_s02_real02_reference.py audit --pcap "C:/captures/202611212141.pcapng" --workbook "协议 (PH20261115).xlsx" --output-dir target/qa/multimode-reference
```

The independent reader derives fields directly from PCAPNG bytes and refuses to
overwrite evidence. The generated manifest can then be passed to the explicit
production backend test via `LANGMUIR_S02_REAL02_MANIFEST`, with
`LANGMUIR_S02_REAL02_INPUT` pointing to the full capture and
`LANGMUIR_S02_REAL02_OUTPUT` naming a new recording directory. The test must be
built without the `mock` feature:

```powershell
cargo test -p langmuir-desktop --no-default-features --locked s02_real02_nonmock_paced_production_full_storage_and_history -- --ignored --nocapture --test-threads=1
```

Use the resulting `production-summary.json` with the reference tool's `verify`
subcommand to independently read back all saved files and both AB history exports.
The original capture is not included in Git; the subset README describes the
smaller sample and how to reproduce it without modifying the original.
