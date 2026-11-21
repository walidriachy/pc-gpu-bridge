# Contributing to PC Bridge

Thanks for helping! Bug reports, docs fixes and pull requests are all welcome.

## Ground rules

* **Beginners are the audience.** Anything a user sees must be in plain English, explain *what* and *why*, and offer a fix when something fails.
* **Never block the user.** If the PC can't do a job, it falls back to the Mac. Don't add paths that hang or fail silently.
* **Stay inside the root.** Agent code must use `safe_path` / `check_argv_paths`, so nothing escapes `PCBridge\`.
* **Edits to user files must be undoable.** Use idempotent changes with marked blocks and a `.pcbridge-backup` copy (see `claude_config.rs`).
* **Keep [ROADMAP.md](ROADMAP.md) current.** A PR that changes behaviour updates the roadmap and the changelog in the same PR.

## Setup

Requirements: Rust stable, Node 20+, Python 3.11+.

```bash
python3 -m venv agent/.venv && agent/.venv/bin/pip install -e "agent[dev]"
(cd app && npm ci && npm run build)
```

## Run every check (the same as CI)

```bash
# Python agent
(cd agent && .venv/bin/ruff check . && .venv/bin/ruff format --check . && .venv/bin/python -m pytest -q)

# UI
(cd app && npm run lint && npm test && npm run build)

# Rust
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Real end-to-end: starts the Python agent and runs the Rust engine against it
bash scripts/e2e-real-agent.sh
```

Run the app with hot reload: `cd app && npx tauri dev`.

## Where things go

| Change | Place |
|---|---|
| A new HTTP endpoint on the PC | `agent/pcbridge_agent/app.py` + a test in `agent/tests/test_api.py` |
| Routing or classification | `crates/bridge-core/src/{router,detect}.rs` + unit tests in the same file |
| A new Claude tool | `crates/bridge-core/src/mcp.rs` (`tool_defs` + `call_tool`) + docs in `docs/USAGE.md` |
| A Mac or Windows system action | `app/src-tauri/src/{mac,win}.rs`, registered in `main.rs` |
| A screen or wizard step | `app/src/pages/*` or `app/src/steps/*` (each step has `what`, `why`, an action and an automatic check) |

## Style

* Rust: `rustfmt` (`max_width = 120`) and `clippy -D warnings`.
* Python: `ruff` (line length 110), with type hints on public functions.
* TypeScript: `eslint` + `tsc --noEmit`, with function components and hooks.
* Commit messages are short, imperative, and say *why* in the body when it isn't obvious.

## Pull requests

1. Fork, then create a branch from `main`.
2. Add or update tests. CI must be green on Linux, macOS and Windows.
3. Update `CHANGELOG.md` (under *Unreleased*), plus `ROADMAP.md` and the docs if behaviour changed.
4. Describe what you tested. Say whether you tried it on a real Mac and PC.

## Releasing (maintainers)

1. Move the *Unreleased* changelog entries under the new version, and bump `version` in `app/package.json`, `app/src-tauri/tauri.conf.json` and the `Cargo.toml` files.
2. Commit, then `git tag vX.Y.Z && git push --tags`. The release workflow builds and uploads both installers.
