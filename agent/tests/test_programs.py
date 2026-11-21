"""Missing-program hints, the install catalog, and where jobs look for their results."""

import asyncio
import os
import sys
from pathlib import Path

import pytest

from pcbridge_agent import jobs, programs
from pcbridge_agent.jobs import JobQueue, changed_files, scan_targets, snapshot

# ---------------- catalog lookups ----------------


@pytest.mark.parametrize(
    ("name", "key"),
    [
        ("blender", "blender"),
        ("Blender", "blender"),
        ("blender.exe", "blender"),
        ("C:\\path\\blender.exe", "blender"),
        ('"C:\\Program Files\\Blender Foundation\\Blender 4.2\\blender.exe"', "blender"),
        ("/usr/bin/ffmpeg", "ffmpeg"),
        ("ffprobe.exe", "ffmpeg"),
        ("npm.cmd", "node"),
        ("npx", "node"),
        ("Node.js", "node"),
        ("torch", "pytorch"),
        ("torchvision", "pytorch"),
        ("torchaudio", "pytorch"),
        ("PyTorch (GPU)", "pytorch"),
        ("Docker Desktop", "docker"),
        ("git", "git"),
        ("ollama", "ollama"),
    ],
)
def test_lookup_finds_catalog_entry(name, key):
    prog = programs.lookup(name)
    assert prog is not None and prog.key == key


@pytest.mark.parametrize("name", ["", "photoshop", "python", "C:\\Windows\\cmd.exe", "../blender/x"])
def test_lookup_unknown(name):
    assert programs.lookup(name) is None


def test_every_alias_points_at_the_catalog():
    assert all(key in programs.CATALOG for key in programs.ALIASES.values())


def test_missing_module():
    assert programs.missing_module("ModuleNotFoundError: No module named 'torch'").key == "pytorch"
    assert programs.missing_module("No module named 'torchvision.models'").key == "pytorch"
    assert programs.missing_module("No module named 'numpy'") is None
    assert programs.missing_module("all good") is None
    # A name cut off at the end of an output chunk must not match a shorter name.
    assert programs.missing_module("No module named 'tor") is None


def test_missing_in_output_recognises_shell_errors():
    win = "'blender' is not recognized as an internal or external command,\r\noperable program"
    assert programs.missing_in_output(win).key == "blender"
    assert programs.missing_in_output("sh: 1: blender: not found").key == "blender"
    assert programs.missing_in_output("bash: line 1: ffprobe: command not found").key == "ffmpeg"
    assert programs.missing_in_output("'mytool' is not recognized as an internal or external command") is None
    assert programs.missing_in_output("frame 10: not found in cache") is None


@pytest.mark.parametrize(
    "text",
    [
        "warning: ffmpeg: not found in cache",
        "KeyError: 'blender': not found",
        "fatal: remote error: git: not found",
        "Error: docker: not found",
        "publish: ffmpeg: not found",
        "error: node: not found in registry",
    ],
)
def test_ordinary_errors_naming_a_program_are_not_missing_programs(text):
    assert programs.missing_in_output(text) is None


@pytest.mark.parametrize(
    ("text", "key"),
    [
        ("./render.sh: line 3: blender: command not found", "blender"),
        ("/bin/sh: 1: git: not found\n", "git"),
        ("/usr/bin/bash: line 1: ffmpeg: command not found\r\n", "ffmpeg"),
        ("Traceback\nzsh: command not found: npm\nmore", "node"),
        ("The term 'docker' is not recognized as a name of a cmdlet, function", "docker"),
    ],
)
def test_real_shell_messages_are_recognised(text, key):
    assert programs.missing_in_output(text).key == key


def test_hint_mentions_pc_install_and_the_key():
    for prog in programs.CATALOG.values():
        text = programs.hint(prog)
        assert "pc_install" in text and f'"{prog.key}"' in text and prog.name in text
        assert text.endswith("\n")


def test_install_argv_winget(monkeypatch):
    monkeypatch.setattr(programs, "which", lambda cmd: "C:\\winget.exe" if cmd == "winget" else None)
    argv = programs.install_argv(programs.CATALOG["blender"])
    assert argv[:5] == ["C:\\winget.exe", "install", "--id", "BlenderFoundation.Blender", "-e"]
    assert "--silent" in argv and "--accept-package-agreements" in argv
    assert "--disable-interactivity" in argv  # never wait for an answer nobody can type


def test_install_argv_pip_uses_the_pcs_python(monkeypatch):
    monkeypatch.setattr(programs, "pc_python", lambda: "C:\\Python312\\python.exe")
    argv = programs.install_argv(programs.CATALOG["pytorch"])
    assert argv[:4] == ["C:\\Python312\\python.exe", "-m", "pip", "install"]
    assert {"torch", "torchvision", "torchaudio"} <= set(argv)
    assert argv[argv.index("--index-url") + 1].startswith("https://download.pytorch.org/whl/cu")
    assert "--no-cache-dir" in argv  # no second copy of the 3 GB download on the C: drive


def test_install_argv_explains_what_is_missing(monkeypatch):
    monkeypatch.setattr(programs, "which", lambda cmd: None)
    monkeypatch.setattr(programs, "pc_python", lambda: None)
    with pytest.raises(RuntimeError, match="App Installer"):
        programs.install_argv(programs.CATALOG["ffmpeg"])
    with pytest.raises(RuntimeError, match="Python isn't installed"):
        programs.install_argv(programs.CATALOG["pytorch"])


def test_install_argv_only_ever_installs_catalog_packages(monkeypatch):
    monkeypatch.setattr(programs, "which", lambda cmd: "winget")
    monkeypatch.setattr(programs, "pc_python", lambda: "python")
    for prog in programs.CATALOG.values():
        argv = programs.install_argv(prog)
        assert argv[1:3] == ["install", "--id"] or argv[1:4] == ["-m", "pip", "install"]


def test_winget_already_installed_codes():
    assert programs.already_installed_code(0x8A15002B)
    assert programs.already_installed_code(-1978335189)  # the same code, signed
    assert not programs.already_installed_code(0)
    assert not programs.already_installed_code(1)
    assert not programs.already_installed_code(None)


def test_winget_restart_codes():
    assert programs.restart_to_finish_code(0x8A150109)
    assert programs.restart_to_finish_code(0x8A150109 - (1 << 32))  # signed
    assert programs.restart_to_install_code(0x8A15010A)
    assert not programs.restart_to_finish_code(0x8A15010A) and not programs.restart_to_finish_code(None)


def _install_job(tmp_path):
    return JobQueue(tmp_path).create("install", "install Docker Desktop", ["winget"], tmp_path, gpu=False)


def test_restart_to_finish_counts_as_installed(tmp_path):
    job = _install_job(tmp_path)
    assert JobQueue._install_result(job, 0x8A150109) == 0
    assert job.summary()["restart_needed"] is True
    assert any(e["type"] == "restart_needed" for e in job.events)
    assert "Restart the PC to finish" in "".join(e.get("data", "") for e in job.events)


def test_restart_to_install_is_a_failure_that_says_restart(tmp_path):
    job = _install_job(tmp_path)
    assert JobQueue._install_result(job, 0x8A15010A) == 0x8A15010A
    assert job.summary()["restart_needed"] is False
    assert "Restart the PC, then install it again" in "".join(e.get("data", "") for e in job.events)


def test_is_installed_python_package(monkeypatch):
    monkeypatch.setattr(programs, "pc_python", lambda: sys.executable)
    assert programs.is_installed(programs.Program("j", "JSON", "", 0, "py:json"))
    assert not programs.is_installed(programs.Program("x", "X", "", 0, "py:pcbridge_no_such_module"))
    monkeypatch.setattr(programs, "pc_python", lambda: None)
    assert not programs.is_installed(programs.Program("j", "JSON", "", 0, "py:json"))


def _fake_exe(folder: Path, name: str) -> Path:
    folder.mkdir(parents=True, exist_ok=True)
    exe = folder / (f"{name}.exe" if sys.platform == "win32" else name)
    exe.write_text("")
    exe.chmod(0o755)
    return exe


def test_pc_python_skips_the_store_alias(tmp_path, monkeypatch):
    stub = _fake_exe(tmp_path / "Microsoft" / "WindowsApps", "python")
    real = _fake_exe(tmp_path / "Python312", "python")
    path = os.pathsep.join([str(stub.parent), str(real.parent)])
    monkeypatch.setattr(programs, "system_path", lambda: path)
    assert os.path.normcase(programs.pc_python()) == os.path.normcase(str(real))
    monkeypatch.setattr(programs, "system_path", lambda: str(stub.parent))
    assert programs.pc_python() is None


@pytest.mark.skipif(sys.platform == "win32", reason="uses shell scripts as stand-in Pythons")
def test_pc_python_accepts_a_python_installed_from_the_store(tmp_path, monkeypatch):
    monkeypatch.setattr(programs, "_PROBE_CACHE", {})
    apps = tmp_path / "Microsoft" / "WindowsApps"
    store = _fake_exe(apps, "python")
    store.write_text("#!/bin/sh\necho /real/python\n")  # a real Python answers
    monkeypatch.setattr(programs, "system_path", lambda: str(apps))
    assert programs.pc_python() == str(store)
    # The placeholder that only offers to open the Store fails.
    monkeypatch.setattr(programs, "_PROBE_CACHE", {})
    store.write_text("#!/bin/sh\necho 'Python was not found' >&2\nexit 9009\n")
    assert programs.pc_python() is None


def test_pc_python_skips_the_agents_own_venv(tmp_path, monkeypatch):
    venv = tmp_path / "venv"
    own = _fake_exe(venv / ("Scripts" if sys.platform == "win32" else "bin"), "python")
    real = _fake_exe(tmp_path / "Python312", "python")
    monkeypatch.setattr(sys, "prefix", str(venv))
    monkeypatch.setattr(sys, "base_prefix", str(tmp_path / "base"))
    monkeypatch.setattr(programs, "system_path", lambda: os.pathsep.join([str(own.parent), str(real.parent)]))
    assert os.path.normcase(programs.pc_python()) == os.path.normcase(str(real))


def test_find_command_finds_blender_outside_path(tmp_path, monkeypatch):
    base = tmp_path / "Program Files"
    for version in ("Blender 4.2", "Blender 4.10"):
        folder = base / "Blender Foundation" / version
        folder.mkdir(parents=True)
        (folder / "blender.exe").write_text("")
    monkeypatch.setenv("ProgramFiles", str(base))
    found = programs.find_command("blender")
    assert found and Path(found).parent.name == "Blender 4.10"  # newest version
    assert programs.find_command("blender.exe") == found
    assert programs.find_command("ffmpeg") is None
    assert programs.find_command(str(base / "blender")) is None  # only bare names
    monkeypatch.setattr(programs, "which", lambda cmd: None)
    assert programs.is_installed(programs.CATALOG["blender"])
    monkeypatch.delenv("ProgramFiles")
    assert programs.find_command("blender") is None


# ---------------- hints from real jobs ----------------


def _run_job(tmp_path, argv, cwd=None, kind="run"):
    async def main():
        q = JobQueue(tmp_path)
        job = q.create(kind, "j", argv, cwd or tmp_path)
        await q.submit(job)
        async for _ in job.follow():
            pass
        return job

    return asyncio.run(main())


def _stderr(job):
    return "".join(e.get("data", "") for e in job.events if e["type"] == "stderr")


def test_missing_blender_gets_the_install_hint(tmp_path):
    exe = tmp_path / "not-installed" / ("blender.exe" if sys.platform == "win32" else "blender")
    job = _run_job(tmp_path, [str(exe), "-b", "scene.blend"])
    assert job.exit_code == 127 and job.state == "failed"
    err = _stderr(job)
    assert "program not found" in err and "pc_install" in err and '"blender"' in err
    types = [e["type"] for e in job.events]
    missing = [e for e in job.events if e["type"] == "missing"]
    assert missing == [{"type": "missing", "program": "blender", "name": "Blender", "t": missing[0]["t"]}]
    assert types.index("missing") < types.index("exit")
    assert job.summary()["missing"] == "blender"


def test_unknown_missing_program_has_no_install_hint(tmp_path):
    job = _run_job(tmp_path, ["pcbridge-definitely-not-a-program"])
    assert job.exit_code == 127
    assert "pc_install" not in _stderr(job)
    assert not [e for e in job.events if e["type"] == "missing"]


NO_TORCH = "import sys; sys.stderr.write(\"ModuleNotFoundError: No module named 'torch'\\n\"); sys.exit(1)"


def test_python_job_missing_torch_gets_the_pytorch_hint(tmp_path, monkeypatch):
    monkeypatch.setattr(programs, "pc_python", lambda: sys.executable)  # the job runs the PC's Python
    job = _run_job(tmp_path, [sys.executable, "-c", NO_TORCH])
    assert job.exit_code == 1
    assert '"pytorch"' in _stderr(job) and "PyTorch" in _stderr(job)
    assert [e["program"] for e in job.events if e["type"] == "missing"] == ["pytorch"]


def test_job_with_another_python_is_told_to_pip_install_into_that_python(tmp_path, monkeypatch):
    """pc_install fills the PC's main Python: a venv/conda job must not be sent there."""
    monkeypatch.setattr(programs, "pc_python", lambda: str(tmp_path / "Python312" / "python.exe"))
    job = _run_job(tmp_path, [sys.executable, "-c", NO_TORCH])
    err = _stderr(job)
    assert job.exit_code == 1 and "pc_install " not in err.replace("pc_install only", "")
    assert f'"{sys.executable}" -m pip install torch' in err
    assert not [e for e in job.events if e["type"] == "missing"] and job.summary()["missing"] is None


def test_which_pythons_count_as_the_pcs_own(tmp_path, monkeypatch):
    pc = _fake_exe(tmp_path / "Python312", "python")
    monkeypatch.setattr(programs, "pc_python", lambda: str(pc))
    assert jobs._uses_pc_python(str(pc))
    assert jobs._uses_pc_python("C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe")  # a shell
    assert not jobs._uses_pc_python("C:/proj/.venv/Scripts/python.exe")
    assert not jobs._uses_pc_python("C:/Windows/py.exe")
    assert not jobs._uses_pc_python("C:/miniconda3/python3.11.exe")
    venv = tmp_path / "proj" / ".venv"
    whisper = _fake_exe(venv / "Scripts", "whisper")
    (venv / "pyvenv.cfg").write_text("home = C:/Python312\n")
    assert not jobs._uses_pc_python(str(whisper))  # a venv's own command
    python = "python.exe" if sys.platform == "win32" else "python"
    assert jobs._job_python(str(whisper)) == str(whisper.parent / python)
    assert jobs._job_python("C:/proj/.venv/Scripts/python.exe") == "C:/proj/.venv/Scripts/python.exe"


def test_module_error_split_across_output_chunks_is_still_recognised(tmp_path, monkeypatch):
    monkeypatch.setattr(programs, "pc_python", lambda: sys.executable)
    code = (
        "import sys, time\n"
        "sys.stderr.write('Traceback\\nModuleNotFoundError: No mod'); sys.stderr.flush(); time.sleep(0.3)\n"
        "sys.stderr.write(\"ule named 'torch'\\n\"); sys.exit(1)\n"
    )
    job = _run_job(tmp_path, [sys.executable, "-c", code])
    assert [e["program"] for e in job.events if e["type"] == "missing"] == ["pytorch"]


@pytest.mark.skipif(sys.platform == "win32", reason="uses a shell script as a stand-in winget")
def test_install_does_not_wait_for_gpu_jobs_and_gives_up_when_stuck(tmp_path, monkeypatch):
    """A winget install stuck on a "Yes" prompt nobody clicks: GPU jobs still run, then it stops."""
    monkeypatch.setattr(jobs, "WATCH_INTERVAL_S", 0.05)
    monkeypatch.setattr(jobs, "WINGET_QUIET_NOTICE_S", 0.2)
    monkeypatch.setattr(jobs, "WINGET_QUIET_LIMIT_S", 1.0)
    winget = _fake_exe(tmp_path / "bin", "winget")
    winget.write_text("#!/bin/sh\necho 'Starting package install...'\nexec sleep 30\n")

    async def main():
        q = JobQueue(tmp_path, max_parallel=1)
        install = q.create("install", "install Blender", [str(winget)], tmp_path, gpu=False)
        await q.submit(install)
        await asyncio.sleep(0.3)
        run = q.create("run", "encode", [sys.executable, "-c", "print('encoded')"], tmp_path)
        await q.submit(run)
        async for _ in run.follow():
            pass
        assert install.state == "running"  # the GPU job didn't wait for the install
        assert q.queue_depth() == 0  # installs don't make the PC look busy to the Mac's router
        async for _ in install.follow():
            pass
        return install, run

    install, run = asyncio.run(main())
    assert run.state == "done"
    assert install.state == "failed"
    err = _stderr(install)
    assert "someone there needs to click Yes" in err  # the early notice
    assert "stopped the install" in err and "click “Yes”" in err
    assert install.outputs == []


def test_gpu_job_does_not_hold_up_an_install(tmp_path):
    async def main():
        q = JobQueue(tmp_path, max_parallel=1)
        render = q.create("run", "render", [sys.executable, "-c", "import time; time.sleep(3)"], tmp_path)
        await q.submit(render)
        await asyncio.sleep(0.2)
        argv = [sys.executable, "-c", "print('ok')"]
        install = q.create("install", "install Git", argv, tmp_path, gpu=False)
        await q.submit(install)
        async for _ in install.follow():
            pass
        state = render.state
        await q.cancel(render.id)
        async for _ in render.follow():
            pass
        return install, state

    install, render_state = asyncio.run(main())
    assert install.state == "done" and render_state == "running"


def test_no_hint_when_the_job_succeeds_anyway(tmp_path):
    code = "import sys; sys.stderr.write(\"No module named 'torch', using the CPU path\\n\")"
    job = _run_job(tmp_path, [sys.executable, "-c", code])
    assert job.exit_code == 0 and "pc_install" not in _stderr(job)


def test_utf8_split_across_reads_is_not_mangled(tmp_path):
    code = (
        "import sys, time\n"
        "data = 'é'.encode()\n"
        "sys.stdout.buffer.write(data[:1]); sys.stdout.flush(); time.sleep(0.3)\n"
        "sys.stdout.buffer.write(data[1:] + b'\\n')\n"
    )
    job = _run_job(tmp_path, [sys.executable, "-c", code])
    assert "".join(e.get("data", "") for e in job.events if e["type"] == "stdout").strip() == "é"


# ---------------- where a job looks for its results ----------------


def _touch(path: Path) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("x")
    return path


def test_snapshot_depth_limit(tmp_path):
    _touch(tmp_path / "top.txt")
    _touch(tmp_path / "a" / "b" / "two.txt")
    _touch(tmp_path / "a" / "b" / "c" / "three.txt")
    assert {Path(p).name for p in snapshot(tmp_path)} == {"top.txt", "two.txt"}
    assert {Path(p).name for p in snapshot(tmp_path, depth=0)} == {"top.txt"}
    assert {Path(p).name for p in snapshot(tmp_path, depth=None)} == {"top.txt", "two.txt", "three.txt"}


def test_snapshot_extra_folders_and_time_budget(tmp_path):
    cwd, other = tmp_path / "proj", tmp_path / "renders"
    _touch(cwd / "in.txt")
    _touch(other / "x" / "y" / "frame.png")
    _touch(other / "x" / "y" / "z" / "too-deep.png")
    found = {Path(p).name for p in snapshot(cwd, [(other, 2)])}
    assert found == {"in.txt", "frame.png"}
    assert {Path(p).name for p in snapshot(cwd, [(other, 0)])} == {"in.txt"}
    assert snapshot(cwd, [(other, 2)], budget=0) == {}


def test_scan_targets_follow_path_arguments_inside_the_root(tmp_path):
    root = tmp_path / "PCBridge"
    cwd = root / "workspace" / "proj"
    cwd.mkdir(parents=True)
    media = _touch(root / "workspace" / "media" / "in.mp4").parent
    renders = root / "workspace" / "renders"
    renders.mkdir()
    (root / "workspace" / "out").mkdir()
    (tmp_path / "outside").mkdir()
    argv = [
        "ffmpeg",
        "-y",
        "-i",
        str(media / "in.mp4"),  # absolute file -> its folder, no subfolders
        "--frames=../renders",  # key=value, relative folder -> scanned like the job's folder
        "..\\out\\clip.mp4",  # Windows separators, file not written yet -> its folder
        "local.mp4",  # in the job's own folder, which is scanned anyway
        str(tmp_path / "outside" / "x.mp4"),  # outside the PCBridge root
        "//render_",  # Blender-relative
        "https://example.com/a.mp4",
        "missing/dir/x.mp4",  # parent doesn't exist
    ]
    got = scan_targets(cwd, argv, root)
    assert got == [
        (media.resolve(), 0),
        (renders.resolve(), 2),
        ((root / "workspace" / "out").resolve(), 0),
    ]


def test_job_finds_outputs_in_folders_its_arguments_name_but_not_deep_in_its_own(tmp_path):
    root = tmp_path
    cwd = root / "workspace" / "BigShared"
    _touch(cwd / "a" / "b" / "c" / "d" / "old.txt")
    out = root / "workspace" / "exports" / "2026" / "nov" / "day" / "result.txt"
    code = (
        "import pathlib, sys\n"
        "for p in sys.argv[1:] + ['a/b/c/deep.txt', 'near.txt']:\n"
        "    pathlib.Path(p).parent.mkdir(parents=True, exist_ok=True)\n"
        "    pathlib.Path(p).write_text('x')\n"
    )
    job = _run_job(root, [sys.executable, "-c", code, str(out)], cwd=cwd)
    assert job.exit_code == 0
    assert sorted(job.outputs) == [
        "workspace/BigShared/near.txt",
        "workspace/exports/2026/nov/day/result.txt",
    ]
    # The file three levels down still exists (Syncthing brings it to the Mac), it's just not listed.
    assert (cwd / "a" / "b" / "c" / "deep.txt").exists()


def test_blender_double_slash_output_next_to_the_blend_file_is_found(tmp_path):
    """blender -b <abs>/Clients/Proj/scene.blend -o //renders/f_#### run from the workspace."""
    root = tmp_path / "PCBridge"
    ws = root / "workspace"
    proj = ws / "Clients" / "Proj"
    blend = _touch(proj / "scene.blend")
    argv = ["blender", "-b", str(blend), "-o", "//renders/f_####", "-a"]
    targets = scan_targets(ws, argv, root)
    assert (proj.resolve(), 2) in targets
    before = snapshot(ws, targets)
    _touch(proj / "renders" / "f_0001.png")
    after = snapshot(ws, targets)
    assert [p.name for p in changed_files(before, after, 0)] == ["f_0001.png"]
    # A deeper existing output folder is followed too; with no .blend, "//" means the job's folder.
    deep = proj / "out" / "a" / "b"
    deep.mkdir(parents=True)
    assert (deep.resolve(), 2) in scan_targets(ws, [*argv[:4], "//out/a/b/"], root)
    assert scan_targets(proj, ["blender", "-o", "//frames/f_"], root) == []
