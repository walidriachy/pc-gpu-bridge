"""ComfyUI shares the GPU slot with other jobs; job summaries report compute time, queue wait, cache."""

import asyncio
import json
import sys
import time
import urllib.error
import urllib.request

import httpx
import pytest
from fastapi.testclient import TestClient

import pcbridge_agent.app as appmod
from pcbridge_agent.app import create_app
from pcbridge_agent.config import Settings
from pcbridge_agent.jobs import JobQueue

PY = sys.executable
DONE = {
    "outputs": {"9": {"images": [{"filename": "img_0001.png", "subfolder": "", "type": "output"}]}},
    "status": {"status_str": "success", "completed": True, "messages": []},
}
FAILED = {
    "outputs": {},
    "status": {
        "status_str": "error",
        "completed": False,
        "messages": [
            ["execution_start", {"prompt_id": "p1"}],
            ["execution_error", {"node_type": "KSampler", "exception_message": "CUDA out of memory"}],
        ],
    },
}


class FakeComfy:
    """Stands in for ComfyUI's HTTP API (patched over ``_http_json``)."""

    def __init__(self, done_after: int | None = 1, entry: dict | None = None, up: bool = True):
        self.done_after = done_after
        self.entry = entry or DONE
        self.up = up
        self.prompt_error: Exception | None = None
        self.running = False  # whether /queue lists our prompt as running
        self.calls: list[tuple[float, str, object]] = []
        self.polls = 0
        self.history_errors = 0  # how many /history polls fail before ComfyUI answers again

    def paths(self) -> list[str]:
        return [p for _, p, _ in self.calls]

    def when(self, path: str) -> float:
        return next(t for t, p, _ in self.calls if p == path)

    def __call__(self, url, body=None, timeout=600):
        path = url.removeprefix(appmod.COMFY_URL)
        if body is not None and path == "/queue":
            path = "/queue (delete)"
        self.calls.append((time.time(), path, body))
        if not self.up:
            raise urllib.error.URLError("connection refused")
        if path == "/system_stats":
            return {"system": {"os": "nt"}}
        if path == "/prompt":
            if self.prompt_error:
                raise self.prompt_error
            return {"prompt_id": "p1", "number": 1, "node_errors": {}}
        if path.startswith("/history/"):
            if self.history_errors:
                self.history_errors -= 1
                raise TimeoutError("timed out")
            self.polls += 1
            if self.done_after is not None and self.polls > self.done_after:
                return {"p1": self.entry}
            return {}
        if path == "/queue":
            return {"queue_running": [[1, "p1", {}, {}, []]] if self.running else [], "queue_pending": []}
        if path == "/interrupt":
            self.running = False
            return None
        if path == "/queue (delete)":
            return None
        raise AssertionError(f"unexpected ComfyUI call {path}")


@pytest.fixture()
def env(tmp_path, monkeypatch):
    monkeypatch.setattr(appmod, "COMFY_POLL_S", 0.02)
    fake = FakeComfy()
    monkeypatch.setattr(appmod, "_http_json", fake)

    def retrieve(url, dest):
        assert url.startswith(appmod.COMFY_URL + "/view?")
        open(dest, "wb").write(b"png")
        return dest, None

    monkeypatch.setattr(urllib.request, "urlretrieve", retrieve)
    s = Settings.load(tmp_path / "PCBridge")
    app = create_app(s)
    client = TestClient(app)
    r = client.post("/pair", json={"code": app.state.pairing.ensure_code(), "device_name": "test-mac"})
    client.headers["Authorization"] = f"Bearer {r.json()['token']}"
    (s.root_path / "workspace").mkdir(parents=True, exist_ok=True)
    return {"client": client, "app": app, "queue": app.state.queue, "fake": fake, "root": s.root_path}


def workflow() -> dict:
    return {"workflow": {"3": {"class_type": "KSampler", "inputs": {}}}}


def comfy_job(env):
    return next(j for j in env["queue"].jobs.values() if j.kind == "comfy")


def assert_slot_free(env):
    q = env["queue"]
    assert not q.running and not q.waiting and q.queue_depth() == 0


def assert_finished(job, state):
    assert job.state == state and job.finished and job.exit_code is not None
    assert job.events[-1]["type"] == "exit" and job.events[-1]["state"] == state
    assert json.loads((job.folder / "job.json").read_text())["state"] == state


async def until(cond, timeout=10.0):
    end = time.monotonic() + timeout
    while not cond():
        assert time.monotonic() < end, "timed out waiting"
        await asyncio.sleep(0.02)


def async_client(env):
    headers = {"Authorization": env["client"].headers["Authorization"]}
    transport = httpx.ASGITransport(app=env["app"])
    return httpx.AsyncClient(transport=transport, base_url="http://agent", headers=headers, timeout=60)


# ---------------- ComfyUI and the GPU slot ----------------


def test_comfy_generate_saves_images_and_reports_timings(env):
    r = env["client"].post("/comfy/generate", json=workflow())
    assert r.status_code == 200, r.text
    body = r.json()
    assert body["kind"] == "comfy" and body["state"] == "done" and body["exit_code"] == 0
    folder = comfy_job(env).folder.relative_to(env["root"]).as_posix()
    assert body["outputs"] == [f"{folder}/output/img_0001.png"]
    assert (env["root"] / body["outputs"][0]).read_bytes() == b"png"
    assert body["cached"] is False and body["queue_secs"] < 1 and 0 <= body["duration"] < 5
    assert env["fake"].paths()[:2] == ["/system_stats", "/prompt"]
    assert_finished(comfy_job(env), "done")
    assert_slot_free(env)


def test_comfy_waits_for_the_gpu_job_holding_the_slot(env):
    q, fake = env["queue"], env["fake"]

    async def main():
        async with async_client(env) as c:
            render = {"argv": [PY, "-c", "import time; time.sleep(1.5)"], "cwd": "workspace", "stream": False}
            run_task = asyncio.create_task(c.post("/run", json=render))
            await until(lambda: q.running)
            comfy_task = asyncio.create_task(c.post("/comfy/generate", json=workflow()))
            await until(lambda: any(q.jobs[j].kind == "comfy" for j in q.waiting))
            st = (await c.get("/status")).json()
            comfy = comfy_job(env)
            waiting_summary = comfy.summary()
            assert "/prompt" not in fake.paths()  # nothing reached ComfyUI while the render runs
            return st, waiting_summary, await run_task, await comfy_task

    st, waiting_summary, run_r, comfy_r = asyncio.run(main())
    assert st["queue_depth"] == 2 and st["waiting"] == 1
    assert waiting_summary["state"] == "queued" and waiting_summary["duration"] is None
    run_job, comfy = run_r.json(), comfy_r.json()
    assert run_job["state"] == comfy["state"] == "done"
    assert fake.when("/prompt") >= run_job["finished"]  # ComfyUI started only once the slot was free
    assert comfy["started"] >= run_job["finished"]
    assert comfy["queue_secs"] >= 0.5 and comfy["duration"] < comfy["queue_secs"]
    assert_slot_free(env)


def test_gpu_job_waits_for_comfy_holding_the_slot(env):
    q, fake = env["queue"], env["fake"]
    fake.done_after = 40  # about a second of "generating"

    async def main():
        async with async_client(env) as c:
            comfy_task = asyncio.create_task(c.post("/comfy/generate", json=workflow()))
            await until(lambda: "/prompt" in fake.paths())
            render = {"argv": [PY, "-c", "print('rendered')"], "cwd": "workspace", "stream": False}
            run_task = asyncio.create_task(c.post("/run", json=render))
            await until(lambda: q.waiting)
            assert q.queue_depth() == 2
            return await comfy_task, await run_task

    comfy_r, run_r = asyncio.run(main())
    assert comfy_r.json()["state"] == run_r.json()["state"] == "done"
    assert run_r.json()["started"] >= comfy_r.json()["finished"]
    assert_slot_free(env)


def test_comfy_not_running_is_503_before_queueing(env):
    env["fake"].up = False
    r = env["client"].post("/comfy/generate", json=workflow())
    assert r.status_code == 503 and "ComfyUI is not running" in r.json()["detail"]
    assert env["fake"].paths() == ["/system_stats"]
    assert not [j for j in env["queue"].jobs.values() if j.kind == "comfy"]
    assert_slot_free(env)


def test_comfy_going_away_after_the_check_is_503_and_frees_the_slot(env):
    env["fake"].prompt_error = urllib.error.URLError("connection refused")
    r = env["client"].post("/comfy/generate", json=workflow())
    assert r.status_code == 503 and "ComfyUI is not running" in r.json()["detail"]
    job = comfy_job(env)
    assert_finished(job, "failed")
    assert job.exit_code == 1 and "ComfyUI is not running" in "".join(e.get("data", "") for e in job.events)
    assert_slot_free(env)


def test_comfy_timeout_is_504_stops_the_prompt_and_frees_the_slot(env, monkeypatch):
    fake = env["fake"]
    fake.done_after = None  # never finishes
    fake.running = True
    r = env["client"].post("/comfy/generate", json={**workflow(), "timeout": 0.3})
    assert r.status_code == 504 and "did not finish in time" in r.json()["detail"]
    interrupts = [b for _, p, b in fake.calls if p == "/interrupt"]
    assert interrupts == [{"prompt_id": "p1"}]  # only our prompt, and the slot waited for it to stop
    assert fake.paths()[-1] == "/queue"
    job = comfy_job(env)
    assert_finished(job, "failed")
    assert 0.3 <= job.duration() < 5
    assert_slot_free(env)


def test_comfy_missed_answers_while_generating_keep_the_slot(env):
    env["fake"].history_errors = 3  # ComfyUI busy, e.g. loading a big checkpoint
    r = env["client"].post("/comfy/generate", json=workflow())
    assert r.status_code == 200 and r.json()["state"] == "done", r.text
    assert env["fake"].paths().count("/interrupt") == 0
    assert_slot_free(env)


def test_comfy_that_stops_answering_is_stopped_before_the_slot_is_freed(env, monkeypatch):
    monkeypatch.setattr(appmod, "COMFY_UNANSWERED_S", 0.2)
    fake = env["fake"]
    fake.history_errors = 10**6
    fake.running = True
    r = env["client"].post("/comfy/generate", json=workflow())
    assert r.status_code == 503 and "stopped answering" in r.json()["detail"]
    assert sum(p.startswith("/history/") for p in fake.paths()) > 1, "retried before giving up"
    assert [b for _, p, b in fake.calls if p == "/interrupt"] == [{"prompt_id": "p1"}]
    job = comfy_job(env)
    assert fake.when("/interrupt") <= job.finished and fake.paths()[-1] == "/queue"
    assert_finished(job, "failed")
    assert_slot_free(env)


def test_comfy_unexpected_error_fails_the_job_and_frees_the_slot(env, monkeypatch):
    def broken(url, dest):
        raise RuntimeError("disk full")

    monkeypatch.setattr(urllib.request, "urlretrieve", broken)
    r = env["client"].post("/comfy/generate", json=workflow())
    assert r.status_code == 200 and r.json()["state"] == "failed"
    job = comfy_job(env)
    assert_finished(job, "failed")
    assert "disk full" in "".join(e.get("data", "") for e in job.events if e["type"] == "stderr")
    assert_slot_free(env)


def test_comfy_workflow_error_is_a_failed_job(env):
    env["fake"].entry = FAILED
    r = env["client"].post("/comfy/generate", json=workflow())
    body = r.json()
    assert r.status_code == 200 and body["state"] == "failed" and body["outputs"] == []
    job = comfy_job(env)
    assert "KSampler: CUDA out of memory" in "".join(e.get("data", "") for e in job.events)
    assert_slot_free(env)


def test_comfy_cancelled_while_waiting_never_reaches_comfyui(env):
    q, fake = env["queue"], env["fake"]

    async def main():
        async with async_client(env) as c:
            render = {"argv": [PY, "-c", "import time; time.sleep(30)"], "cwd": "workspace", "stream": False}
            run_task = asyncio.create_task(c.post("/run", json=render))
            await until(lambda: q.running)
            comfy_task = asyncio.create_task(c.post("/comfy/generate", json=workflow()))
            await until(lambda: q.waiting)
            assert (await c.post(f"/jobs/{comfy_job(env).id}/cancel")).status_code == 200
            comfy_r = await comfy_task
            render_id = next(iter(q.running))
            await c.post(f"/jobs/{render_id}/cancel")
            await run_task
            return comfy_r

    comfy_r = asyncio.run(main())
    assert comfy_r.status_code == 200 and comfy_r.json()["state"] == "cancelled"
    assert comfy_r.json()["duration"] is None and comfy_r.json()["queue_secs"] > 0
    assert "/prompt" not in fake.paths()
    assert_finished(comfy_job(env), "cancelled")
    assert_slot_free(env)


def test_comfy_cancelled_while_generating_drops_the_prompt(env):
    q, fake = env["queue"], env["fake"]
    fake.done_after = None

    async def main():
        async with async_client(env) as c:
            comfy_task = asyncio.create_task(c.post("/comfy/generate", json=workflow()))
            await until(lambda: fake.polls > 0)
            await c.post(f"/jobs/{comfy_job(env).id}/cancel")
            return await comfy_task

    r = asyncio.run(main())
    assert r.status_code == 200 and r.json()["state"] == "cancelled"
    assert [b for _, p, b in fake.calls if p == "/queue (delete)"] == [{"delete": ["p1"]}]
    assert_finished(comfy_job(env), "cancelled")
    assert_slot_free(env)
    assert q.queue_depth() == 0


# ---------------- the queue itself ----------------


def test_waiter_whose_task_is_cancelled_leaves_the_queue(tmp_path):
    async def main():
        q = JobQueue(tmp_path, max_parallel=1)
        long = q.create("run", "long", [PY, "-c", "import time; time.sleep(30)"], tmp_path)
        await q.submit(long)
        await until(lambda: q.running)
        other = q.create("comfy", "comfy-generate", ["comfyui"], tmp_path)
        q.waiting.append(other.id)
        task = asyncio.create_task(q._acquire(other))
        await asyncio.sleep(0.1)
        assert q.queue_depth() == 2
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
        assert other.id not in q.waiting and q.queue_depth() == 1
        await q.cancel(long.id)
        async for _ in long.follow():
            pass
        assert not q.running

    asyncio.run(main())


# ---------------- summary: duration, queue_secs, cached ----------------


def test_duration_is_compute_time_only(tmp_path, monkeypatch):
    def slow_collect(self, changed, out_dir):
        time.sleep(0.6)
        return []

    monkeypatch.setattr(JobQueue, "_collect_outputs", slow_collect)

    async def main():
        q = JobQueue(tmp_path, max_parallel=1)
        j = q.create("run", "quick", [PY, "-c", "print(1)"], tmp_path)
        await q.submit(j)
        async for _ in j.follow():
            pass
        return j

    j = asyncio.run(main())
    s = j.summary()
    assert s["state"] == "done" and s["cached"] is False
    assert s["duration"] < j.finished - j.started - 0.5  # collecting outputs isn't compute time
    assert s["duration"] == round(j.exited - j.started, 3) and s["queue_secs"] < 0.5
    exit_ev = j.events[-1]
    assert exit_ev["duration"] == s["duration"] and exit_ev["queue_secs"] == s["queue_secs"]
    assert exit_ev["cached"] is False


def test_queued_job_reports_its_wait_separately(tmp_path):
    async def main():
        q = JobQueue(tmp_path, max_parallel=1)
        first = q.create("run", "first", [PY, "-c", "import time; time.sleep(1)"], tmp_path)
        second = q.create("run", "second", [PY, "-c", "print(2)"], tmp_path)
        await q.submit(first)
        await q.submit(second)
        await asyncio.sleep(0.4)
        waiting = second.summary()
        for j in (first, second):
            async for _ in j.follow():
                pass
        return first, second, waiting

    first, second, waiting = asyncio.run(main())
    assert waiting["state"] == "queued" and waiting["duration"] is None and waiting["queue_secs"] >= 0.3
    s = second.summary()
    assert s["queue_secs"] >= 0.8 and s["queue_secs"] == round(second.acquired - second.queued, 3)
    assert s["duration"] < s["queue_secs"]
    assert second.events[-1]["queue_secs"] == s["queue_secs"]
    assert first.summary()["queue_secs"] < 0.5


def test_cancelled_while_waiting_reports_no_compute(tmp_path):
    async def main():
        q = JobQueue(tmp_path, max_parallel=1)
        long = q.create("run", "long", [PY, "-c", "import time; time.sleep(30)"], tmp_path)
        wait = q.create("run", "wait", [PY, "-c", "print(1)"], tmp_path)
        await q.submit(long)
        await q.submit(wait)
        await asyncio.sleep(0.3)
        await q.cancel(wait.id)
        await q.cancel(long.id)
        for j in (long, wait):
            async for _ in j.follow():
                pass
        return wait

    wait = asyncio.run(main())
    s = wait.summary()
    assert s["state"] == "cancelled" and s["duration"] is None and s["queue_secs"] >= 0.2
    assert wait.events[-1] == {**wait.events[-1], "code": -1, "duration": 0.0, "cached": False}


def test_run_api_reports_compute_queue_and_cache(env):
    c = env["client"]
    ws = env["root"] / "workspace"
    (ws / "in.txt").write_text("abc")
    script = "import pathlib,time;time.sleep(0.3);pathlib.Path('out.txt').write_text('x')"
    body = {"argv": [PY, "-c", script], "cwd": "workspace", "inputs": ["workspace/in.txt"], "stream": False}
    first = c.post("/run", json=body).json()
    assert first["cached"] is False and first["duration"] >= 0.3 and 0 <= first["queue_secs"] < 1
    second = c.post("/run", json=body).json()
    assert second["cached"] is True and second["duration"] == 0 and second["queue_secs"] == 0
    evs = [json.loads(line) for line in c.post("/run", json={**body, "stream": True}).text.splitlines()]
    assert [e["type"] for e in evs][-2:] == ["cached", "exit"]
    assert evs[-1]["cached"] is True and evs[-1]["duration"] == 0 and evs[-1]["queue_secs"] == 0
