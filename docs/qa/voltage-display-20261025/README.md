# Voltage display acceptance evidence, 2026-10-25

These selected JSON summaries, logs and Python audit sources are byte-for-byte copies of the completed local acceptance evidence. No test, Windows build, GUI or scientific-file validation was rerun for this documentation publication.

`final-evidence-manifest.json` is the original acceptance snapshot. Its report and plan hashes refer to the pre-publication backups in the local `target/qa/voltage-20261025/publication-backup/` directory; they intentionally differ from the edited publication documents. Its source and delivered EXE/ZIP hashes still identify the accepted production artifacts. `publication-manifest.json` separately hashes the current report, plan and this selected evidence set, excluding itself.

Full `actual-pcap-reference-frames.jsonl`, original PCAP/TXT, scientific CSV/LPD/LVDSRAW, application source backups and EXE/ZIP are excluded from this directory. Small representative sample values already contained in the audit summaries remain part of the original evidence. Existing screenshots remain in the parent `docs/qa/` directory.

The copied scripts retain their original bytes, local input paths and output guards. They document how acceptance was performed; several require the excluded local scientific inputs and JSONL. Reader helpers are repository QA utilities: `tools/qa/lvds_raw_recovery_reference.py`, `tools/qa/lvds_txt_reference.py`, `tools/qa/lvds_txt_to_pcapng.py` and its `lvds_10bit_reference.py` import. `previous-package.json` preserves the small metadata dependency used by the frozen source check. Do not rerun the old `check_report_evidence.py` to regenerate the original frozen manifest against the edited publication documents.

The original instruction to stop without committing applied to acceptance. The user later explicitly authorized the present GitHub publication.
