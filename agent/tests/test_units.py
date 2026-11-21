import asyncio
import datetime as dt
import os
import subprocess
import sys
from pathlib import Path

import pytest

from pcbridge_agent.cache import ResultCache, cache_key
from pcbridge_agent.cleanup import cleanup_old
from pcbridge_agent.config import PathOutsideRoot, check_argv_paths, safe_path
from pcbridge_agent.gpu import driver_is_recent, parse_nvidia_smi
from pcbridge_agent.jobs import JobQueue, short_name
from pcbridge_agent.memory import memory_info, parse_meminfo
from pcbridge_agent.pairing import MAX_ATTEMPTS, PairingStore

# ---------------- paths ----------------


def test_safe_path_relative_ok(tmp_path):
    assert safe_path(tmp_path, "workspace/a/b.mp4") == (tmp_path / "workspace/a/b.mp4").resolve()
    assert safe_path(tmp_path, "workspace\\a\\b.mp4") == (tmp_path / "workspace/a/b.mp4").resolve()


@pytest.mark.parametrize("bad", ["../etc/passwd", "workspace/../../x", "/etc/passwd"])
def test_safe_path_refuses_escape(tmp_path, bad):
    with pytest.raises(PathOutsideRoot):
        safe_path(tmp_path, bad)


def test_check_argv_paths(tmp_path):
    inside = str(tmp_path / "workspace" / "in.mp4")
    check_argv_paths(tmp_path, ["ffmpeg", "-i", inside, "out.mp4"])
    with pytest.raises(PathOutsideRoot):
        check_argv_paths(tmp_path, ["ffmpeg", "-i", "/etc/passwd"])
    with pytest.raises(PathOutsideRoot):
        check_argv_paths(tmp_path, ["tool", "--out=/tmp/elsewhere"])


# ---------------- pairing ----------------


def test_pairing_flow(tmp_path):
    now = [1000.0]
    store = PairingStore(tmp_path, clock=lambda: now[0])
    code = store.ensure_code()
    assert len(code) == 6 and code.isdigit()
    assert store.redeem("000000" if code != "000000" else "111111", "mac") is None
    token = store.redeem(code, "Walid's MacBook")
    assert token and store.verify(token)
    assert not store.verify("nope")
    assert store.redeem(code, "again") is None  # one-time
    assert store.devices()[0]["name"] == "Walid's MacBook"


def test_pairing_expiry_and_attempts(tmp_path):
    now = [0.0]
    store = PairingStore(tmp_path, clock=lambda: now[0])
    code = store.new_code()
    now[0] = 10_000
    assert store.redeem(code, "x") is None  # expired
    now[0] = 0
    code = store.new_code()
    wrong = "123456" if code != "123456" else "654321"
    for _ in range(MAX_ATTEMPTS):
        store.redeem(wrong, "x")
    assert store.redeem(code, "x") is None  # locked out


def test_local_token(tmp_path):
    store = PairingStore(tmp_path)
    assert store.verify(store.local_token())


# ---------------- gpu / memory ----------------


def test_parse_nvidia_smi():
    out = "NVIDIA GeForce RTX 3090, 37, 4096, 24576, 55, 560.94\n"
    g = parse_nvidia_smi(out)[0]
    assert g["name"] == "NVIDIA GeForce RTX 3090"
    assert g["vram_total_mb"] == 24576 and g["utilization"] == 37
    assert driver_is_recent(g["driver"])
    assert not driver_is_recent("470.10")
    assert parse_nvidia_smi("garbage") == []


def test_parse_meminfo():
    m = parse_meminfo("MemTotal: 32000000 kB\nMemFree: 1000 kB\nMemAvailable: 16000000 kB\n")
    assert m["total_bytes"] == 32000000 * 1024 and m["available_bytes"] == 16000000 * 1024


def test_memory_info_live():
    m = memory_info()
    assert m["total_bytes"] > 0 and 0 <= m["used_percent"] <= 100


# ---------------- cache ----------------


def test_cache_roundtrip_and_cap(tmp_path):
    root = tmp_path
    (root / "workspace").mkdir()
    inp = root / "workspace" / "in.txt"
    inp.write_text("hello")
    out = root / "workspace" / "out.txt"
    out.write_text("result")
    c = ResultCache(root / "cache", root, max_bytes=10_000)
    k = cache_key(["tool", "in.txt"], [inp])
    assert c.get(k) is None
    c.put(k, [out], 0)
    out.unlink()
    assert c.get(k) == ["workspace/out.txt"] and out.read_text() == "result"
    inp.write_text("changed")
    assert cache_key(["tool", "in.txt"], [inp]) != k
    # failed jobs are never cached
    c.put("bad", [out], 1)
    assert c.get("bad") is None
    # cap eviction
    big = root / "workspace" / "big.bin"
    big.write_bytes(b"x" * 20_000)
    c.put("big", [big], 0)
    assert c.size() <= 10_000 or c.get("big") is None


# ---------------- cleanup ----------------


def test_cleanup_old(tmp_path):
    old = tmp_path / "jobs" / "2026-01-01"
    new = tmp_path / "jobs" / "2026-11-20"
    old.mkdir(parents=True)
    new.mkdir(parents=True)
    removed = cleanup_old(tmp_path, 14, today=dt.date(2026, 11, 21))
    assert str(old) in removed and not old.exists() and new.exists()


# ---------------- queue ----------------


def test_short_name():
    assert short_name("Encode My Video!!.mp4") == "encode-my-video-mp4"


def test_queue_fifo_one_at_a_time(tmp_path):
    async def main():
        q = JobQueue(tmp_path, max_parallel=1)
        cwd = tmp_path / "workspace"
        cwd.mkdir()
        py = sys.executable
        jobs = []
        for i in range(3):
            j = q.create("run", f"j{i}", [py, "-c", f"import time;time.sleep(0.2);print({i})"], cwd)
            await q.submit(j)
            jobs.append(j)
        assert q.position(jobs[2].id) >= 1
        for j in jobs:
            async for _ in j.follow():
                pass
        starts = [j.started for j in jobs]
        ends = [j.finished for j in jobs]
        assert starts == sorted(starts)
        assert starts[1] >= ends[0] and starts[2] >= ends[1]  # never overlapping
        assert [j.exit_code for j in jobs] == [0, 0, 0]
        assert (jobs[0].folder / "log.txt").read_text().strip() == "0"

    asyncio.run(main())


def test_queue_cancel_and_exit_codes(tmp_path):
    async def main():
        q = JobQueue(tmp_path)
        cwd = tmp_path
        py = sys.executable
        long = q.create("run", "long", [py, "-c", "import time;time.sleep(30)"], cwd)
        waiting = q.create("run", "wait", [py, "-c", "print(1)"], cwd)
        fail = q.create("run", "fail", [py, "-c", "import sys;sys.exit(3)"], cwd)
        for j in (long, waiting, fail):
            await q.submit(j)
        await asyncio.sleep(0.3)
        assert await q.cancel(waiting.id)
        assert await q.cancel(long.id)
        for j in (long, waiting, fail):
            async for _ in j.follow():
                pass
        assert long.state == "cancelled" and waiting.state == "cancelled"
        assert fail.exit_code == 3 and fail.state == "failed"
        missing = q.create("run", "missing", ["definitely-not-a-program-xyz"], cwd)
        await q.submit(missing)
        async for _ in missing.follow():
            pass
        assert missing.exit_code == 127

    asyncio.run(main())


def test_job_detects_outputs(tmp_path):
    async def main():
        q = JobQueue(tmp_path)
        cwd = tmp_path / "workspace"
        cwd.mkdir()
        j = q.create("run", "w", [sys.executable, "-c", "open('out.txt','w').write('x')"], cwd)
        await q.submit(j)
        async for _ in j.follow():
            pass
        assert j.outputs == ["workspace/out.txt"]
        assert (j.folder / "output" / "out.txt").exists()
        assert Path(j.folder / "job.json").exists()

    asyncio.run(main())


# ---------------- syncthing ----------------


def test_syncthing_calls(tmp_path):
    import json as _json

    from pcbridge_agent.syncthing import Syncthing, folder_id

    calls = []

    class Resp:
        def __init__(self, body):
            self.body = body

        def read(self):
            return self.body

        def __enter__(self):
            return self

        def __exit__(self, *a):
            return False

    def opener(req, timeout=10):
        calls.append(
            (
                req.get_method(),
                req.full_url,
                req.headers.get("X-api-key"),
                _json.loads(req.data) if req.data else None,
            )
        )
        if req.full_url.endswith("/rest/system/status"):
            return Resp(b'{"myID": "PC-ID"}')
        return Resp(b"")

    st = Syncthing("k", opener=opener)
    assert st.device_id() == "PC-ID"
    st.add_device("MAC-ID", "Mac")
    st.share_folder(folder_id("My Videos"), "My Videos", tmp_path / "w" / "My Videos", ["MAC-ID", "PC-ID"])
    assert (tmp_path / "w" / "My Videos").is_dir()
    methods = [(c[0], c[1].split("8385")[1]) for c in calls]
    assert ("PUT", "/rest/config/devices/MAC-ID") in methods
    assert ("PUT", "/rest/config/folders/pcbridge-my-videos") in methods
    assert any(m == "POST" and u.startswith("/rest/db/ignores") for m, u in methods)
    assert all(c[2] == "k" for c in calls)
    folder_body = next(c[3] for c in calls if "/config/folders/" in c[1])
    assert {d["deviceID"] for d in folder_body["devices"]} == {"MAC-ID", "PC-ID"}


def test_resolve_claude_finds_native_install_off_path(tmp_path, monkeypatch):
    from pcbridge_agent.config import resolve_claude

    monkeypatch.setenv("PATH", str(tmp_path / "empty"))
    monkeypatch.setattr(Path, "home", lambda: tmp_path)
    assert resolve_claude("claude") is None
    exe = tmp_path / ".local" / "bin" / "claude.exe"
    exe.parent.mkdir(parents=True)
    exe.write_text("")
    assert os.path.normcase(resolve_claude("claude")) == os.path.normcase(str(exe))
    assert resolve_claude(str(exe)) == str(exe)


def test_merge_path_dedupes_and_keeps_order():
    from pcbridge_agent.config import _merge_path

    sep = os.pathsep
    assert _merge_path([f"/a{sep}/b", f"/b{sep}{sep}/c", '"/d"']) == sep.join(["/a", "/b", "/c", "/d"])


def test_which_sees_programs_added_to_path_after_start(tmp_path, monkeypatch):
    from pcbridge_agent.config import which

    tool = tmp_path / ("pcbridge_fake_tool.bat" if sys.platform == "win32" else "pcbridge_fake_tool")
    tool.write_text("@echo off\n" if sys.platform == "win32" else "#!/bin/sh\n")
    tool.chmod(0o755)
    assert which("pcbridge_fake_tool") is None
    monkeypatch.setenv("PATH", str(tmp_path) + os.pathsep + os.environ.get("PATH", ""))
    assert which("pcbridge_fake_tool")


@pytest.mark.skipif(
    sys.platform != "win32" or not os.environ.get("CI"), reason="edits the Windows registry (CI only)"
)
def test_which_reads_user_path_from_registry(tmp_path):
    """A program installed after the agent started (user PATH changed in the registry) is found."""
    import winreg

    from pcbridge_agent.config import which

    (tmp_path / "pcbridge_reg_tool.bat").write_text("@echo off\n")
    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, "Environment", 0, winreg.KEY_ALL_ACCESS) as key:
        try:
            old, kind = winreg.QueryValueEx(key, "Path")
        except OSError:
            old, kind = None, winreg.REG_EXPAND_SZ
        winreg.SetValueEx(key, "Path", 0, kind, f"{old};{tmp_path}" if old else str(tmp_path))
        try:
            assert str(tmp_path) not in os.environ.get("PATH", "")
            assert which("pcbridge_reg_tool")
        finally:
            if old is None:
                winreg.DeleteValue(key, "Path")
            else:
                winreg.SetValueEx(key, "Path", 0, kind, old)


def test_snapshot_skips_dependency_folders_and_sync_temp_files(tmp_path):
    from pcbridge_agent.jobs import snapshot

    (tmp_path / "node_modules" / "pkg").mkdir(parents=True)
    (tmp_path / "node_modules" / "pkg" / "index.js").write_text("x")
    (tmp_path / ".git").mkdir()
    (tmp_path / ".git" / "HEAD").write_text("x")
    (tmp_path / "sub").mkdir()
    (tmp_path / "sub" / "video.mp4").write_text("x")
    (tmp_path / "~syncthing~clip.mov.tmp").write_text("x")
    (tmp_path / ".syncthing.clip.mov.tmp").write_text("x")
    found = {Path(p).name for p in snapshot(tmp_path)}
    assert found == {"video.mp4"}


def test_changed_files_ignores_files_synced_in_from_the_mac(tmp_path):
    import time

    from pcbridge_agent.jobs import changed_files

    started = time.time()
    old_mtime = int((started - 3600) * 1e9)  # Syncthing keeps the Mac's modification time
    new_mtime = int((started + 1) * 1e9)
    before = {"a.mp4": int((started - 7200) * 1e9)}
    after = {"a.mp4": new_mtime, "synced.mov": old_mtime, "out.mp4": new_mtime}
    assert sorted(p.name for p in changed_files(before, after, started)) == ["a.mp4", "out.mp4"]


def test_agent_keeps_answering_while_a_job_scans_a_big_folder(tmp_path, monkeypatch):
    """A slow folder scan must not block other requests (it froze /health for a minute)."""
    import time

    from pcbridge_agent import jobs as jobs_mod

    real = jobs_mod.snapshot

    def slow_snapshot(*args, **kwargs):
        time.sleep(1.0)
        return real(*args, **kwargs)

    monkeypatch.setattr(jobs_mod, "snapshot", slow_snapshot)

    async def scenario():
        q = JobQueue(tmp_path)
        job = q.create("run", "x", [sys.executable, "-c", "print(1)"], tmp_path)
        await q.submit(job)
        t0 = time.perf_counter()
        await asyncio.sleep(0.2)  # the job is inside its 1 s scan now
        latency = time.perf_counter() - t0
        for _ in range(200):
            if job.state in ("done", "failed"):
                break
            await asyncio.sleep(0.05)
        return latency, job.state

    latency, state = asyncio.run(scenario())
    assert state == "done"
    assert latency < 0.6


def test_check_argv_paths_allows_switches(tmp_path):
    check_argv_paths(tmp_path, ["cmd", "/c", "echo", "hi"])
    check_argv_paths(tmp_path, ["robocopy", "a", "b", "/MIR", "/R:2"])
    with pytest.raises(PathOutsideRoot):
        check_argv_paths(tmp_path, ["type", "/etc/passwd"])


def test_pairing_same_mac_again_replaces_old_entry(tmp_path):
    store = PairingStore(tmp_path)
    old = store.redeem(store.ensure_code(), "Walid's MacBook Pro", "id-1")
    other = store.redeem(store.ensure_code(), "Studio", "id-2")
    new = store.redeem(store.ensure_code(), "Walid's MacBook Pro", "id-1")
    assert [d["name"] for d in store.devices()] == ["Studio", "Walid's MacBook Pro"]
    assert not store.verify(old), "the old token must stop working"
    assert store.verify(new) and store.verify(other)


def test_claim_removes_older_duplicates_of_this_mac(tmp_path):
    store = PairingStore(tmp_path)
    # Three pairings from before 0.1.4 (no device id), plus another Mac.
    t1 = store.redeem(store.ensure_code(), "Walid's MacBook Pro")
    t2 = store.redeem(store.ensure_code(), "Walid's MacBook Pro")
    studio = store.redeem(store.ensure_code(), "Studio")
    t3 = store.redeem(store.ensure_code(), "Walid's MacBook Pro")
    assert store.claim(t3, "", "x") == 0  # no id: nothing happens
    assert store.claim("not-a-token", "id-1", "x") == 0
    assert store.claim(t3, "id-1", "Walid's MacBook Pro") == 2
    assert [(d["name"], d["device_id"]) for d in store.devices()] == [
        ("Studio", None),
        ("Walid's MacBook Pro", "id-1"),
    ]
    assert store.verify(t3) and store.verify(studio)
    assert not store.verify(t1) and not store.verify(t2)
    assert store.claim(t3, "id-1", "Walid's MacBook Pro") == 0  # idempotent


def test_forget_and_remove(tmp_path):
    store = PairingStore(tmp_path)
    a = store.redeem(store.ensure_code(), "A", "id-a")
    b = store.redeem(store.ensure_code(), "B", "id-b")
    assert store.forget(a) and not store.verify(a)
    assert not store.forget(a)
    assert store.remove(0) and not store.verify(b)
    assert not store.remove(0)
    assert store.is_local(store.local_token()) and not store.is_local(b) and not store.is_local(None)


def test_cancel_stops_child_processes_too(tmp_path):
    """powershell/npm/claude start the real work as a child: Cancel must stop it as well."""
    pidfile = tmp_path / "child.pid"
    parent = (
        "import subprocess, sys, time\n"
        "c = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])\n"
        f"open({str(pidfile)!r}, 'w').write(str(c.pid))\n"
        "time.sleep(60)\n"
    )

    def alive(pid: int) -> bool:
        if os.name == "nt":
            out = subprocess.run(["tasklist", "/FI", f"PID eq {pid}"], capture_output=True, text=True).stdout
            return str(pid) in out
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return False
        # A killed child may linger as a zombie until reaped; treat that as stopped.
        try:
            with open(f"/proc/{pid}/stat") as f:
                return f.read().split()[2] != "Z"
        except OSError:
            r = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)], capture_output=True, text=True)
            return bool(r.stdout.strip()) and not r.stdout.strip().startswith("Z")

    async def main():
        q = JobQueue(tmp_path)
        job = q.create("run", "tree", [sys.executable, "-c", parent], tmp_path)
        await q.submit(job)
        for _ in range(100):
            if pidfile.exists() and pidfile.read_text():
                break
            await asyncio.sleep(0.1)
        child = int(pidfile.read_text())
        assert alive(child)
        assert await q.cancel(job.id)

        async def drain():
            async for _ in job.follow():
                pass

        # Before the fix the job only ended when the orphaned child finished (60 s).
        await asyncio.wait_for(drain(), 10)
        assert job.state == "cancelled"
        for _ in range(50):
            if not alive(child):
                break
            await asyncio.sleep(0.1)
        assert not alive(child), "the child process kept running after Cancel"

    asyncio.run(main())
