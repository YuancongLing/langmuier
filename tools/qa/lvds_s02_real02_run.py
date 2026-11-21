"""Run the explicit non-Mock real-PCAP test and record process identity/resource use."""
import argparse
import ctypes
from ctypes import wintypes
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time


class MemoryCounters(ctypes.Structure):
    _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD)] + [
        (name, ctypes.c_size_t) for name in (
            "PeakWorkingSetSize", "WorkingSetSize", "QuotaPeakPagedPoolUsage",
            "QuotaPagedPoolUsage", "QuotaPeakNonPagedPoolUsage", "QuotaNonPagedPoolUsage",
            "PagefileUsage", "PeakPagefileUsage", "PrivateUsage",
        )
    ]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--run-name", required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    evidence = root / "docs/qa/lvds-multitype-s02-real-02"
    base = root / "target/qa/lvds-multitype-s02-real-02"
    base.mkdir(parents=True, exist_ok=True)
    runner = base / (args.run_name + "-tests.exe")
    output = base / args.run_name
    if runner.exists() or output.exists():
        raise FileExistsError("Each run requires unused output and runner paths")
    shutil.copy2(args.binary, runner)
    command = [str(runner), "backend::s02_real02_tests::s02_real02_nonmock_paced_production_full_storage_and_history", "--exact", "--ignored", "--nocapture", "--test-threads=1"]
    env = os.environ.copy()
    env.update(LANGMUIR_S02_REAL02_INPUT=str(root / "202611212141.pcapng"),
               LANGMUIR_S02_REAL02_MANIFEST=str(evidence / "independent-manifest.json"),
               LANGMUIR_S02_REAL02_OUTPUT=str(output))
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    psapi = ctypes.WinDLL("psapi", use_last_error=True)
    kernel.GetProcessTimes.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(wintypes.FILETIME)] * 4
    psapi.GetProcessMemoryInfo.argtypes = [wintypes.HANDLE, ctypes.POINTER(MemoryCounters), wintypes.DWORD]
    started = time.perf_counter()
    with (evidence / (args.run_name + ".log")).open("x", encoding="utf-8") as log:
        process = subprocess.Popen(command, cwd=root, env=env, stdout=log, stderr=subprocess.STDOUT)
        identity = {"pid": process.pid, "exe": str(runner), "exe_sha256": hashlib.sha256(runner.read_bytes()).hexdigest(), "cwd": str(root), "command": command, "input": env["LANGMUIR_S02_REAL02_INPUT"], "output": str(output), "started_unix_ns": time.time_ns()}
        (evidence / (args.run_name + "-identity.json")).write_text(json.dumps(identity, indent=2), encoding="utf-8")
        print(json.dumps(identity), flush=True)
        samples = []
        while process.poll() is None:
            stamps = [wintypes.FILETIME() for _ in range(4)]
            memory = MemoryCounters()
            memory.cb = ctypes.sizeof(memory)
            handle = wintypes.HANDLE(int(process._handle))
            if kernel.GetProcessTimes(handle, *(ctypes.byref(t) for t in stamps)) and psapi.GetProcessMemoryInfo(handle, ctypes.byref(memory), memory.cb):
                cpu = sum((t.dwHighDateTime << 32) | t.dwLowDateTime for t in stamps[2:]) / 10_000_000
                samples.append({"elapsed_seconds": time.perf_counter() - started, "cpu_seconds": cpu, "working_set_bytes": memory.WorkingSetSize, "private_bytes": memory.PrivateUsage})
            time.sleep(1)
    result = dict(identity, exit_code=process.returncode, elapsed_seconds=time.perf_counter() - started, samples=samples, scope="Normal-paced production backend and history test process; not a native GUI benchmark.")
    (evidence / (args.run_name + "-process.json")).write_text(json.dumps(result, indent=2), encoding="utf-8")
    print(json.dumps({"exit_code": process.returncode, "elapsed_seconds": result["elapsed_seconds"], "output": str(output)}), flush=True)
    raise SystemExit(process.returncode)


if __name__ == "__main__":
    main()
