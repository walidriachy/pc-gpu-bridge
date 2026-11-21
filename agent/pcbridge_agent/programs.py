"""Programs PC Bridge knows how to install on the PC, and hints when a job needs a missing one.

Programs are installed in their normal Windows places (``C:\\Program Files``, Python's own
folder…), never inside the PCBridge folder or the shared folders that sync back to the Mac.
Only programs in :data:`CATALOG` can be installed through the agent, never arbitrary commands.
"""

from __future__ import annotations

import glob
import os
import re
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path

from .config import system_path, which


@dataclass(frozen=True)
class Program:
    key: str
    name: str
    why: str
    size_gb: float
    check: str  # executable name, or "py:<module>" for a Python package
    winget: str = ""
    pip: tuple[str, ...] = ()


CATALOG: dict[str, Program] = {
    p.key: p
    for p in (
        Program("ffmpeg", "FFmpeg", "video and audio encoding", 0.3, "ffmpeg", winget="Gyan.FFmpeg"),
        Program(
            "blender",
            "Blender",
            "3D modelling and rendering",
            1.0,
            "blender",
            winget="BlenderFoundation.Blender",
        ),
        Program(
            "pytorch",
            "PyTorch (GPU)",
            "AI in Python: Whisper, Stable Diffusion, your own models",
            # About 3 GB to download and 5 GB once unpacked; pip also needs room for the download
            # while it unpacks, so the free-space check (size + 2 GB) asks for 8 GB.
            6.0,
            "py:torch",
            # torchvision/torchaudio too: their "No module named" errors point here as well.
            pip=(
                "torch",
                "torchvision",
                "torchaudio",
                "--index-url",
                "https://download.pytorch.org/whl/cu128",
            ),
        ),
        Program("ollama", "Ollama", "local AI chat models", 1.0, "ollama", winget="Ollama.Ollama"),
        Program(
            "node",
            "Node.js",
            "JavaScript builds and tests (npm, npx)",
            0.2,
            "node",
            winget="OpenJS.NodeJS.LTS",
        ),
        Program(
            "docker",
            "Docker Desktop",
            "Docker builds and containers",
            3.0,
            "docker",
            winget="Docker.DockerDesktop",
        ),
        Program("git", "Git", "version control", 0.4, "git", winget="Git.Git"),
    )
}

# Command, Python module or everyday name -> catalog key
ALIASES = {
    "ffprobe": "ffmpeg",
    "npm": "node",
    "npx": "node",
    "nodejs": "node",
    "node.js": "node",
    "torch": "pytorch",
    "torchvision": "pytorch",
    "torchaudio": "pytorch",
    **{p.name.lower(): p.key for p in CATALOG.values()},  # "docker desktop", "pytorch (gpu)"…
}

# Installers that don't add the program to PATH: where Windows puts it instead.
# (environment variable holding the base folder, glob below it)
INSTALL_DIRS: dict[str, tuple[tuple[str, str], ...]] = {
    "blender": (("ProgramFiles", "Blender Foundation/Blender*"),),
}

# winget exit codes that mean "already there, nothing to do" (not a failure for the user).
WINGET_ALREADY_INSTALLED = {
    0x8A15002B,  # APPINSTALLER_CLI_ERROR_UPDATE_NOT_APPLICABLE: installed, no newer version
    0x8A150061,  # APPINSTALLER_CLI_ERROR_PACKAGE_ALREADY_INSTALLED
}
# Installed, but Windows has to restart to finish (Docker Desktop, MSI upgrades returning 3010).
WINGET_RESTART_TO_FINISH = 0x8A150109  # APPINSTALLER_CLI_ERROR_INSTALL_REBOOT_REQUIRED_TO_FINISH
# Windows has to restart before this installer can run at all.
WINGET_RESTART_TO_INSTALL = 0x8A15010A  # APPINSTALLER_CLI_ERROR_INSTALL_REBOOT_REQUIRED_TO_INSTALL

# A Python job's "ModuleNotFoundError: No module named 'torch'" (or 'torch.cuda'). The closing
# quote or dot is required so a name cut in half at an output chunk boundary never matches.
_NO_MODULE = re.compile(r"No module named '([A-Za-z0-9_]+)[.']")
# A shell that couldn't find a program the job's script calls. Only real shell messages count
# (a whole line, starting with the shell or script name), never text that merely contains
# "<name>: not found", such as "warning: ffmpeg: not found in cache".
_NO_COMMAND = re.compile(
    # cmd.exe and PowerShell (5: "as the name of a cmdlet", 7: "as a name of a cmdlet")
    r"'([^'\s]+)' is not recognized as (?:an internal or external command|(?:the|a) name of a cmdlet)"
    # sh/dash "sh: 1: ffmpeg: not found", bash "bash: line 1: ffmpeg: command not found",
    # a script "./render.sh: line 3: ffmpeg: command not found", Git Bash "/usr/bin/bash: ..."
    r"|^(?:\S*/)?(?:(?:ba|da|k|z)?sh|\S+\.sh)(?:: (?:line )?\d+)?: "
    r"([^\s:/\\]+): (?:command )?not found[ \t\r]*$"
    # zsh "zsh: command not found: ffmpeg"
    r"|^zsh: command not found: ([^\s:/\\]+)[ \t\r]*$",
    re.MULTILINE,
)


def lookup(name: str) -> Program | None:
    """Catalog entry for a program, command (``blender.exe``, ``C:\\...\\blender.exe``) or module."""
    text = name.strip().strip("\"'").replace("\\", "/").rstrip("/")
    key = text.rsplit("/", 1)[-1].lower()
    for ext in (".exe", ".cmd", ".bat"):
        key = key.removesuffix(ext)
    key = ALIASES.get(key, key)
    return CATALOG.get(key)


def missing_module(text: str) -> Program | None:
    """The catalog entry for a ``ModuleNotFoundError`` in a Python job's output."""
    for m in _NO_MODULE.finditer(text):
        prog = lookup(m.group(1))
        if prog:
            return prog
    return None


def missing_in_output(text: str) -> Program | None:
    """A catalog program the job's output says is missing (Python module or shell command)."""
    prog = missing_module(text)
    if prog:
        return prog
    for m in _NO_COMMAND.finditer(text):
        prog = lookup(m.group(1) or m.group(2) or m.group(3))
        if prog:
            return prog
    return None


def hint(program: Program) -> str:
    return (
        f"PC Bridge: {program.name} isn't installed on the PC. "
        f'Ask Claude to install it (tool pc_install, program "{program.key}"), '
        f"or on the PC open PC Bridge → Settings → Programs → Install.\n"
    )


def pip_hint(program: Program, python: str) -> str:
    """For a job that ran another Python (a venv, conda, ``py -3.11``…) than the PC's own one."""
    packages = " ".join(program.pip)
    return (
        f"PC Bridge: {program.name} isn't installed in the Python this job used ({python}). "
        "pc_install only adds it to the PC's main Python, so install it into this one instead: "
        f'"{python}" -m pip install {packages}\n'
    )


def _natural(path: str) -> list:
    return [int(t) if t.isdigit() else t.lower() for t in re.split(r"(\d+)", path)]


def find_command(cmd: str) -> str | None:
    """Full path of a catalog program installed where it isn't on PATH (Blender), else None.

    Only bare command names (``blender``, ``blender.exe``) are looked up; the newest version wins.
    """
    if "/" in cmd or "\\" in cmd:
        return None
    name = cmd.lower().removesuffix(".exe")
    found: list[str] = []
    for env, pattern in INSTALL_DIRS.get(name, ()):
        base = os.environ.get(env)
        if not base:
            continue
        for folder in glob.glob(os.path.join(glob.escape(base), pattern)):
            for exe in (f"{name}.exe", name):
                candidate = os.path.join(folder, exe)
                if os.path.isfile(candidate):
                    found.append(candidate)
                    break
    return max(found, key=_natural) if found else None


def locate(cmd: str) -> str | None:
    """Where a command is: on the PC's current PATH, or a known install folder."""
    return which(cmd) or find_command(cmd)


_PROBE_CACHE: dict[str, tuple[bool, float]] = {}
_PROBE_RETRY_S = 300.0  # a stub can become a real Python (installed from the Store) later


def _answers(python: str) -> bool:
    """True when ``python`` really runs Python code (cached; a "no" is checked again later)."""
    key = os.path.normcase(python)
    cached = _PROBE_CACHE.get(key)
    if cached and (cached[0] or time.monotonic() - cached[1] < _PROBE_RETRY_S):
        return cached[0]
    try:
        r = subprocess.run(
            [python, "-c", "import sys;print(sys.executable)"], capture_output=True, text=True, timeout=15
        )
        ok = r.returncode == 0 and bool(r.stdout.strip())
    except (OSError, subprocess.TimeoutExpired):
        ok = False
    _PROBE_CACHE[key] = (ok, time.monotonic())
    return ok


def _is_store_stub(path: str) -> bool:
    """The Microsoft Store's "python.exe" placeholder only offers to open the Store.

    A Python installed from the Store sits behind the same kind of alias in a ``WindowsApps``
    folder, so those are tried: the placeholder fails (or prints nothing), a real Python answers.
    """
    if "windowsapps" not in path.replace("\\", "/").lower().split("/"):
        return False
    return not _answers(path)


def _in_agent_venv(path: str) -> bool:
    if sys.prefix == sys.base_prefix:
        return False
    # Compare the folders themselves (no resolve): a venv's python is often a link to the real one.
    here = os.path.normcase(os.path.abspath(path))
    return Path(here).is_relative_to(os.path.normcase(os.path.abspath(sys.prefix)))


def pc_python() -> str | None:
    """The PC's own Python, the one jobs run as ``python`` (never the agent's private venv).

    Skips the Microsoft Store placeholder, which only opens the Store (a Python installed from
    the Store is used).
    """
    for folder in system_path().split(os.pathsep):
        if not folder:
            continue
        for name in ("python", "python3"):
            found = shutil.which(name, path=folder)
            if found and not _is_store_stub(found) and not _in_agent_venv(found):
                return found
    return None


def is_installed(program: Program) -> bool:
    if program.check.startswith("py:"):
        py = pc_python()
        if not py:
            return False
        module = program.check[3:]
        code = f"import importlib.util,sys;sys.exit(0 if importlib.util.find_spec('{module}') else 1)"
        try:
            return subprocess.run([py, "-c", code], capture_output=True, timeout=30).returncode == 0
        except (OSError, subprocess.TimeoutExpired):
            return False
    return locate(program.check) is not None


def install_argv(program: Program) -> list[str]:
    """The exact install command (winget or pip). Raises ``RuntimeError`` with a plain message."""
    if program.pip:
        py = pc_python()
        if not py:
            raise RuntimeError(
                "Python isn't installed on the PC yet. On the PC, open PC Bridge and run the setup's "
                "“Install the tools” step, then try again."
            )
        return [
            py,
            "-m",
            "pip",
            "install",
            "--upgrade",
            "--disable-pip-version-check",
            "--progress-bar",
            "off",
            # No second copy of the (GB-sized) download in pip's cache on the C: drive.
            "--no-cache-dir",
            *program.pip,
        ]
    winget = which("winget")
    if not winget:
        raise RuntimeError(
            "The PC is missing Windows' “App Installer” (winget), which PC Bridge uses to install "
            "programs. On the PC, install “App Installer” from the Microsoft Store, then try again."
        )
    return [
        winget, "install", "--id", program.winget, "-e", "--silent", "--disable-interactivity",
        "--accept-package-agreements", "--accept-source-agreements",
    ]  # fmt: skip


def already_installed_code(code: int | None) -> bool:
    """True for winget's "already installed / no newer version" exit codes (signed or unsigned)."""
    return code is not None and (code & 0xFFFFFFFF) in WINGET_ALREADY_INSTALLED


def _winget_code(code: int | None) -> int | None:
    return None if code is None else code & 0xFFFFFFFF


def restart_to_finish_code(code: int | None) -> bool:
    """winget: installed, but the PC must restart to finish."""
    return _winget_code(code) == WINGET_RESTART_TO_FINISH


def restart_to_install_code(code: int | None) -> bool:
    """winget: the PC must restart before this program can be installed."""
    return _winget_code(code) == WINGET_RESTART_TO_INSTALL


def system_drive() -> Path:
    if sys.platform == "win32":
        return Path(os.environ.get("SystemDrive", "C:") + "\\")
    return Path.home()


def free_gb(path: Path | None = None) -> float:
    try:
        return shutil.disk_usage(path or system_drive()).free / 1e9
    except OSError:
        return -1.0
