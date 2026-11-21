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


# ---------------- programs on the PC ----------------


def test_programs_list_shape(env, monkeypatch):
    from pcbridge_agent import programs

    monkeypatch.setattr(programs, "is_installed", lambda p: p.key == "git")
    body = env["client"].get("/programs").json()
    assert set(body) == {"programs", "free_gb", "drive"}
    keys = [p["key"] for p in body["programs"]]
    assert {"ffmpeg", "blender", "pytorch", "ollama", "node", "docker", "git"} <= set(keys)
    for p in body["programs"]:
        assert set(p) == {"key", "name", "why", "size_gb", "installed"}
        assert p["installed"] is (p["key"] == "git")
    assert isinstance(body["free_gb"], float) and body["drive"]
    assert TestClient(env["app"]).get("/programs").status_code == 401


def test_install_unknown_program_is_404(env):
    r = env["client"].post("/programs/install", json={"program": "photoshop"})
    assert r.status_code == 404
    assert "photoshop" in r.json()["detail"] and "blender" in r.json()["detail"]


def test_install_refused_when_disk_is_low(env, monkeypatch):
    from pcbridge_agent import programs

    monkeypatch.setattr(programs, "free_gb", lambda path=None: 1.0)
    called = []
    monkeypatch.setattr(programs, "install_argv", lambda p: called.append(p) or ["x"])
    r = env["client"].post("/programs/install", json={"program": "pytorch"})
    assert r.status_code == 507
    detail = r.json()["detail"]
    assert "PyTorch" in detail and "1.0 GB free" in detail and "8 GB needed" in detail
    assert not called  # nothing was started


def test_install_explains_missing_winget(env, monkeypatch):
    from pcbridge_agent import programs

    monkeypatch.setattr(programs, "free_gb", lambda path=None: 500.0)
    monkeypatch.setattr(programs, "which", lambda cmd: None)
    r = env["client"].post("/programs/install", json={"program": "blender"})
    assert r.status_code == 503 and "App Installer" in r.json()["detail"]


def test_install_job_streams_and_exits_0(env, monkeypatch):
    from pcbridge_agent import programs

    monkeypatch.setattr(programs, "free_gb", lambda path=None: 500.0)
    monkeypatch.setattr(programs, "install_argv", lambda p: [sys.executable, "-c", "print('ok')"])
    r = env["client"].post("/programs/install", json={"program": "Blender.exe"})
    assert r.status_code == 200
    evs = ndjson(r)
    assert evs[0]["type"] == "job" and evs[-1]["type"] == "exit"
    assert evs[-1]["code"] == 0 and evs[-1]["state"] == "done"
    assert "ok" in "".join(e.get("data", "") for e in evs if e["type"] == "stdout")
    job = env["client"].get("/jobs").json()[0]
    assert job["kind"] == "install" and job["name"] == "install Blender" and job["missing"] is None


def test_install_clicked_twice_follows_the_same_job(env, monkeypatch):
    """Two requests at the same moment (double-click, or Claude and the PC app) share one install."""
    import asyncio
    import time

    import httpx

    from pcbridge_agent import programs

    def slow_free_gb(path=None):
        time.sleep(0.3)  # both requests are past the start of the handler before either job exists
        return 500.0

    monkeypatch.setattr(programs, "free_gb", slow_free_gb)
    argv = [sys.executable, "-c", "import time; time.sleep(1)"]
    monkeypatch.setattr(programs, "install_argv", lambda p: argv)
    headers = {"Authorization": env["client"].headers["Authorization"]}

    async def main():
        transport = httpx.ASGITransport(app=env["app"])
        async with httpx.AsyncClient(transport=transport, base_url="http://agent", headers=headers) as c:
            body = {"program": "git", "stream": False}
            return await asyncio.gather(
                c.post("/programs/install", json=body), c.post("/programs/install", json=body)
            )

    first, second = asyncio.run(main())
    assert first.status_code == second.status_code == 200
    assert first.json()["id"] == second.json()["id"]
    installs = [j for j in env["client"].get("/jobs").json() if j["kind"] == "install"]
    assert len(installs) == 1


def test_status_lists_programs_like_the_programs_page(env, monkeypatch):
    """Blender installed outside PATH counts, and PyTorch and Git are reported too."""
    from pcbridge_agent import programs

    monkeypatch.setattr(programs, "locate", lambda cmd: "C:/Blender/blender.exe" if cmd == "blender" else "")
    monkeypatch.setattr(programs, "is_installed", lambda p: p.key == "pytorch")
    tools = env["client"].get("/status").json()["tools"]
    assert tools["blender"] is True and tools["pytorch"] is True
    assert tools["git"] is False and tools["ffmpeg"] is False
