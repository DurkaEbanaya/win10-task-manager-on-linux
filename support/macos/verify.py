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
    assert data["cpu"].get("darwin_cpu") is True
    assert data["cpu"]["thread_count_valid"] is True
    assert data["cpu"]["total_thread_count"] > 0
    assert data["cpu"]["darwin_open_file_count"] > 0
    assert data["cpu"]["socket_count"] > 0
    for level in data["cpu"]["performance_levels"]:
        assert level["physical_core_count"] > 0
        assert 0 <= level["index"] < 16
        clock = level.get("active_frequency_mhz")
        assert clock is None or 0 < clock < 10000
        minimum, maximum = level.get("minimum_frequency_mhz"), level.get("maximum_frequency_mhz")
        assert (minimum is None and maximum is None) or (
            minimum is not None and maximum is not None and 0 < minimum <= maximum < 10000
        )
    assert data["mem_info"]["sample_valid"] is True
    assert data["mem_info"]["mem_total"] > 0
    assert data["mem_info"]["darwin_accounting"] is True
    assert data["mem_info"].get("darwin_pressure_level") in (None, 1, 2, 4)
    for sensor in data["temperatures"]:
        value = sensor.get("input_millicelsius")
        assert value is None or 0 < value <= 150000
    assert data["running_processes"], "no readable processes"
    assert any(p["usage_stats"]["memory_valid"] for p in data["running_processes"].values())
    assert all(p["usage_stats"]["network_valid"] is False for p in data["running_processes"].values())
    assert data["network_unavailable"] is True
    assert data["disks_info"], "no mounted volumes"
    assert all(s["actions_supported"] is False for s in data["user_services"].values())
print("Native live telemetry smoke passed (private snapshots removed).")
