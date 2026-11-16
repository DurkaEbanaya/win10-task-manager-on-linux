#!/usr/bin/env python3
"""Native integration smoke test; private process snapshots never become artifacts."""
import json
import pathlib
import subprocess
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[2]
APP = ROOT / "target/macos/Task Manager.app/Contents/MacOS"
for name in ("taskmanager", "missioncenter-magpie"):
    description = subprocess.check_output(["file", str(APP / name)], text=True)
    assert "Mach-O 64-bit executable arm64" in description, description

private = ROOT / "testkit/fixtures/live"
private.mkdir(parents=True, exist_ok=True)
with tempfile.TemporaryDirectory(prefix="macos-smoke-", dir=private) as directory:
    subprocess.run(
        [str(APP / "taskmanager"), "--no-config", "--dump-readings", "5", directory],
        check=True, timeout=60,
    )
    snapshots = sorted(pathlib.Path(directory).glob("readings-*.json"))
    assert len(snapshots) == 5, "incomplete live collection"
    data = json.loads(snapshots[-1].read_text())
    assert data["cpu"]["usage_valid"] is True
    assert 0 <= data["cpu"]["total_usage_percent"] <= 100
    assert data["cpu"]["core_usage_percent"]
    assert data["cpu"]["physical_core_count"] > 0
    for level in data["cpu"]["performance_levels"]:
        assert level["physical_core_count"] > 0
        assert 0 <= level["index"] < 16
    assert data["mem_info"]["sample_valid"] is True
    assert data["mem_info"]["mem_total"] > 0
    assert data["mem_info"]["darwin_accounting"] is True
    assert data["running_processes"], "no readable processes"
    assert any(p["usage_stats"]["memory_valid"] for p in data["running_processes"].values())
    assert all(p["usage_stats"]["network_valid"] is False for p in data["running_processes"].values())
    assert data["network_unavailable"] is True
    assert data["disks_info"], "no mounted volumes"
    assert all(s["actions_supported"] is False for s in data["user_services"].values())
print("Native live telemetry smoke passed (private snapshots removed).")
