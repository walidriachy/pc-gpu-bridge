import asyncio
import datetime as dt
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
