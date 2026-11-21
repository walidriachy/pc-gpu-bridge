"""Settings and the strict PCBridge root folder.

Everything the agent writes lives under ONE root (default ``D:\\PCBridge``, fallback ``C:\\PCBridge``).
Any path that would escape that root is refused with :class:`PathOutsideRoot`.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import sys
from dataclasses import asdict, dataclass, field
from pathlib import Path, PurePosixPath, PureWindowsPath

SUBDIRS = ("workspace", "jobs", "worker", "cache", "models", "logs", "config")


class PathOutsideRoot(ValueError):
    """Raised when a path would point outside the PCBridge root."""


def _merge_path(parts: list[str]) -> str:
    seen: set[str] = set()
    out: list[str] = []
    for part in parts:
        for entry in part.split(os.pathsep):
            entry = entry.strip().strip('"')
            key = os.path.normcase(os.path.normpath(entry)) if entry else ""
            if entry and key not in seen:
                seen.add(key)
                out.append(entry)
    return os.pathsep.join(out)


def _registry_path() -> list[str]:
    """Machine + user PATH exactly as Windows stores them right now (with %VARS% expanded)."""
    if sys.platform != "win32":
        return []
    import winreg

    keys = (
        (winreg.HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment"),
        (winreg.HKEY_CURRENT_USER, "Environment"),
    )
    parts: list[str] = []
    for hive, sub in keys:
        try:
            with winreg.OpenKey(hive, sub) as key:
                value, _ = winreg.QueryValueEx(key, "Path")
                parts.append(winreg.ExpandEnvironmentStrings(str(value)))
        except OSError:
            continue
    return parts


def system_path() -> str:
    """The PATH to look programs up in.

    The agent can run for days, so its own PATH goes stale: anything installed after it started
    (FFmpeg via winget, Claude Code, Blender…) would look "not found". On Windows we re-read the
    registry every time, and add the folders winget and Claude's native installer use.
    """
    extra: list[str] = []
    if sys.platform == "win32":
        extra.append(str(Path.home() / ".local" / "bin"))
        local = os.environ.get("LOCALAPPDATA")
        if local:
            extra.append(str(Path(local) / "Microsoft" / "WinGet" / "Links"))
    return _merge_path([*_registry_path(), os.environ.get("PATH", ""), *extra])


def which(cmd: str) -> str | None:
    """``shutil.which`` against the fresh PATH (see :func:`system_path`)."""
    return shutil.which(cmd, path=system_path())


def resolve_claude(cmd: str = "claude") -> str | None:
    """Full path of Claude Code, also when its native installer's folder (~/.local/bin) isn't on PATH."""
    found = which(cmd)
    if found:
        return found
    if Path(cmd).is_file():
        return cmd
    local_bin = Path.home() / ".local" / "bin"
    for name in ("claude.exe", "claude"):
        if (local_bin / name).is_file():
            return str(local_bin / name)
    return None


def default_root() -> Path:
    """Pick the root folder: env override, then D:\\PCBridge if D: exists, else C:\\PCBridge."""
    env = os.environ.get("PCBRIDGE_ROOT")
    if env:
        return Path(env)
    if sys.platform == "win32":
        return Path("D:/PCBridge") if Path("D:/").exists() else Path("C:/PCBridge")
    return Path.home() / "PCBridge"


@dataclass
class Settings:
    root: str = ""
    port: int = 8765
    max_parallel_jobs: int = 1
    cleanup_days: int = 14
    cache_max_gb: float = 20.0
    allowlist: list[str] = field(default_factory=list)  # empty = allow everything
    delegate_args: list[str] = field(
        default_factory=lambda: [
            "--permission-mode",
            "acceptEdits",
            "--allowedTools",
            "Bash,Read,Write,Edit,Glob,Grep",
        ]
    )
    claude_cmd: str = "claude"
    nvidia_smi_cmd: str = "nvidia-smi"
    bind_hosts: list[str] = field(default_factory=list)  # empty = 127.0.0.1 + Tailscale IP
    mac_address: str = ""  # Ethernet MAC for Wake-on-LAN (set by the Windows app); empty = auto
    syncthing_api_key: str = ""
    syncthing_url: str = "http://127.0.0.1:8385"

    @property
    def root_path(self) -> Path:
        return Path(self.root).resolve()

    def ensure_dirs(self) -> None:
        for sub in SUBDIRS:
            (self.root_path / sub).mkdir(parents=True, exist_ok=True)

    # ---- persistence -------------------------------------------------
    @classmethod
    def load(cls, root: Path | None = None) -> Settings:
        root = (root or default_root()).resolve()
        path = root / "config" / "settings.json"
        data: dict = {}
        if path.exists():
            try:
                data = json.loads(path.read_text("utf-8"))
            except json.JSONDecodeError:
                data = {}
        known = {k: v for k, v in data.items() if k in cls.__dataclass_fields__}
        s = cls(**known)
        s.root = str(root)
        s.ensure_dirs()
        return s

    def save(self) -> None:
        path = self.root_path / "config" / "settings.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(asdict(self), indent=2), "utf-8")


_WIN_ABS = re.compile(r"^[A-Za-z]:[\\/]")


_SWITCH = re.compile(r"^/[A-Za-z?][A-Za-z0-9?-]{0,7}(:[^/\\]*)?$")


def looks_absolute(value: str) -> bool:
    """True if a string looks like an absolute Windows or POSIX path."""
    return bool(_WIN_ABS.match(value)) or value.startswith("\\\\") or value.startswith("/")


def safe_path(root: Path, candidate: str | os.PathLike[str]) -> Path:
    """Resolve ``candidate`` (relative to root, or absolute) and refuse anything outside root."""
    root = Path(root).resolve()
    text = str(candidate)
    if not text or text in (".", "./"):
        return root
    if looks_absolute(text):
        p = Path(text)
    else:
        # Accept both separators from the Mac side.
        parts = PureWindowsPath(text).parts if "\\" in text else PurePosixPath(text).parts
        p = root.joinpath(*parts)
    resolved = p.resolve()
    try:
        resolved.relative_to(root)
    except ValueError as exc:
        raise PathOutsideRoot(f"Refusing path outside PCBridge root: {text}") from exc
    return resolved


def check_argv_paths(root: Path, argv: list[str]) -> None:
    """Every absolute-path-looking argument (including ``key=C:\\...``) must sit inside the root."""
    for arg in argv[1:]:
        for piece in (arg, arg.split("=", 1)[-1]):
            if _SWITCH.match(piece):
                continue  # cmd /c, robocopy /MIR, ...: a switch, not a path
            if looks_absolute(piece) and not piece.startswith("//"):
                safe_path(root, piece)
