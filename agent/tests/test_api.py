"""End-to-end API tests with a mock 'GPU' program and a mock Claude worker."""

import json
import sys
from pathlib import Path

import pytest
from fastapi.testclient import TestClient

from pcbridge_agent.app import create_app
from pcbridge_agent.config import Settings

FAKE_CLAUDE = """
import json, sys
task = sys.argv[sys.argv.index('-p') + 1]
print(json.dumps({"type": "system", "subtype": "init"}))
open('answer.txt', 'w').write(task.upper())
print(json.dumps({"type": "result", "result": "done: " + task}))
"""

FAKE_SMI = """
import sys
print("NVIDIA GeForce RTX 3090, 12, 1024, 24576, 40, 560.94")
"""


@pytest.fixture()
def env(tmp_path):
    (tmp_path / "bin").mkdir()
    claude = tmp_path / "bin" / "fake_claude.py"
    claude.write_text(FAKE_CLAUDE)
    smi = tmp_path / "bin" / "fake_smi.py"
    smi.write_text(FAKE_SMI)
    root = tmp_path / "PCBridge"
    s = Settings.load(root)
    app = create_app(s)
    # Point the external tools at our fakes.
    s.claude_cmd = sys.executable
    s.delegate_args = []
    client = TestClient(app)
    code = app.state.pairing.ensure_code()
    r = client.post("/pair", json={"code": code, "device_name": "test-mac"})
    assert r.status_code == 200, r.text
    token = r.json()["token"]
    client.headers["Authorization"] = f"Bearer {token}"
    return {"client": client, "app": app, "root": s.root_path, "claude": claude, "smi": smi, "settings": s}


def ndjson(resp):
    return [json.loads(line) for line in resp.text.splitlines() if line.strip()]


def test_health_and_auth(env):
    anon = TestClient(env["app"])
    assert anon.get("/health").json()["ok"]
    assert anon.get("/status").status_code == 401
    assert anon.post("/pair", json={"code": "999999x"}).status_code == 403
    st = env["client"].get("/status").json()
    assert st["ok"] and st["devices"][0]["name"] == "test-mac"
    assert st["memory"]["total_bytes"] > 0
    assert env["client"].get("/memory").json()["total_bytes"] > 0


def test_peer_memory_reported_by_mac(env):
    c = env["client"]
    assert c.get("/status").json()["peer_memory"] is None
    assert TestClient(env["app"]).post("/peer/memory", json={}).status_code == 401
    c.post("/peer/memory", json={"total_bytes": 16, "available_bytes": 3, "pressure": "warn", "junk": 1})
    pm = c.get("/status").json()["peer_memory"]
    assert pm["available_bytes"] == 3 and pm["pressure"] == "warn" and "junk" not in pm and pm["ts"] > 0


def test_run_streams_and_exit_code(env):
    c = env["client"]
    (env["root"] / "workspace" / "proj").mkdir(parents=True)
    r = c.post(
        "/run",
        json={
            "argv": [
                sys.executable,
                "-c",
                "import sys;print('hello');sys.stderr.write('warn\\n');sys.exit(4)",
            ],
            "cwd": "workspace/proj",
            "name": "test job",
        },
    )
    evs = ndjson(r)
    types = [e["type"] for e in evs]
    assert types[0] == "job" and "started" in types and types[-1] == "exit"
    assert "hello" in "".join(e.get("data", "") for e in evs if e["type"] == "stdout")
    assert "warn" in "".join(e.get("data", "") for e in evs if e["type"] == "stderr")
    assert evs[-1]["code"] == 4
    jobs = c.get("/jobs").json()
    assert jobs[0]["exit_code"] == 4 and "test-job" in jobs[0]["folder"]


def test_run_cache_hit(env):
    c = env["client"]
    ws = env["root"] / "workspace"
    (ws / "in.txt").write_text("abc")
    script = "import pathlib;pathlib.Path('out.txt').write_text(pathlib.Path('in.txt').read_text()[::-1])"
    body = {
        "argv": [sys.executable, "-c", script],
        "cwd": "workspace",
        "inputs": ["workspace/in.txt"],
        "stream": False,
    }
    first = c.post("/run", json=body).json()
    assert first["exit_code"] == 0 and not first["cached"]
    (ws / "out.txt").unlink()
    second = c.post("/run", json=body).json()
    assert second["cached"] and (ws / "out.txt").read_text() == "cba"


def test_strict_root_refusals(env):
    c = env["client"]
    assert c.post("/run", json={"argv": ["ls"], "cwd": "../.."}).status_code == 403
    assert c.post("/run", json={"argv": ["cat", "/etc/passwd"]}).status_code == 403
    assert c.get("/files/pull", params={"path": "../secret"}).status_code == 403


def test_allowlist(env):
    env["settings"].allowlist = ["ffmpeg"]
    r = env["client"].post("/run", json={"argv": [sys.executable, "-c", "1"]})
    assert r.status_code == 403
    env["settings"].allowlist = []


def test_files_push_pull(env):
    c = env["client"]
    r = c.post("/files/push", data={"path": "workspace/up/hello.txt"}, files={"file": ("h.txt", b"hi there")})
    assert r.json()["bytes"] == 8
    assert c.get("/files/pull", params={"path": "workspace/up/hello.txt"}).content == b"hi there"
    assert c.get("/files/pull", params={"path": "workspace/nope.txt"}).status_code == 404
    assert c.get("/files/stat", params={"path": "workspace/up/hello.txt"}).json()["size"] == 8
    assert c.get("/files/stat", params={"path": "workspace/none"}).json() == {"exists": False}


def test_delegate_real_stream_json(env, tmp_path):
    """Use a wrapper so 'claude' is a real executable script emitting stream-json."""
    c = env["client"]
    if sys.platform == "win32":
        wrapper = tmp_path / "claude.cmd"
        wrapper.write_text(f'@"{sys.executable}" "{env["claude"]}" %*\n')
    else:
        wrapper = tmp_path / "claude"
        wrapper.write_text(f'#!/bin/sh\nexec "{sys.executable}" "{env["claude"]}" "$@"\n')
        wrapper.chmod(0o755)
    env["settings"].claude_cmd = str(wrapper)
    evs = ndjson(c.post("/delegate", json={"task": "say hi"}))
    claude_events = [e["event"] for e in evs if e["type"] == "claude"]
    assert claude_events[-1]["result"] == "done: say hi"
    assert evs[-1]["code"] == 0
    job = c.get("/jobs").json()[0]
    assert Path(job["folder"]).exists()
    answer = env["root"] / "worker"
    assert any(p.name == "answer.txt" for p in answer.rglob("*"))


def test_gpu_endpoint_with_fake_smi(env, tmp_path):
    if sys.platform == "win32":
        wrapper = tmp_path / "smi.cmd"
        wrapper.write_text(f'@"{sys.executable}" "{env["smi"]}" %*\n')
    else:
        wrapper = tmp_path / "smi"
        wrapper.write_text(f'#!/bin/sh\nexec "{sys.executable}" "{env["smi"]}" "$@"\n')
        wrapper.chmod(0o755)
    env["settings"].nvidia_smi_cmd = str(wrapper)
    g = env["client"].get("/gpu").json()
    assert g["available"] and g["gpus"][0]["vram_total_mb"] == 24576 and g["driver_ok"]
    env["settings"].nvidia_smi_cmd = "definitely-missing-smi"
    assert env["client"].get("/gpu").json()["available"] is False


def test_cancel_unknown(env):
    assert env["client"].post("/jobs/nope/cancel").status_code == 404


def test_syncthing_not_configured(env):
    assert env["client"].get("/syncthing/info").status_code == 503


def test_new_pairing_code_needs_auth(env):
    anon = TestClient(env["app"])
    assert anon.post("/pair/new").status_code == 401
    code = env["client"].post("/pair/new").json()["code"]
    assert code == env["client"].get("/pair/code").json()["code"]
