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


def resolve_claude(cmd: str = "claude") -> str | None:
    """Full path of Claude Code, also when its native installer's folder (~/.local/bin) isn't on PATH."""
    found = shutil.which(cmd)
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
            if looks_absolute(piece) and not piece.startswith("//"):
                safe_path(root, piece)
