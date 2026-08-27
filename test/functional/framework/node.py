"""Launching the real binary, and reading what it says.

Set AVICOIN_BIN to test a binary built elsewhere; otherwise the debug build is
rebuilt and used.
"""

import os
import shutil
import subprocess
import tempfile
import threading
import time
from functools import cache
from pathlib import Path
from typing import List, Optional

from .p2p import IMPATIENCE, PATIENCE

REPO_ROOT = Path(__file__).resolve().parents[3]
DEFAULT_BINARY = REPO_ROOT / "target" / "debug" / "avicoin"


@cache
def binary_path() -> Path:
    override = os.environ.get("AVICOIN_BIN")
    if override:
        path = Path(override)
        if not path.exists():
            raise FileNotFoundError(f"AVICOIN_BIN={override} does not exist")
        return path

    if shutil.which("cargo") is None:
        raise RuntimeError(
            "cargo is not on PATH; build the node first and set AVICOIN_BIN"
        )

    # Always build, never "build if missing". An existing binary may predate the
    # change under test -- `cargo clippy` alone leaves a stale one behind -- and a
    # suite that silently tests the wrong binary is the failure this whole
    # arrangement exists to prevent. Cargo is a no-op when it is already current.
    subprocess.run(["cargo", "build", "--quiet"], cwd=REPO_ROOT, check=True)

    if not DEFAULT_BINARY.exists():
        raise RuntimeError(f"no binary at {DEFAULT_BINARY} after cargo build")

    return DEFAULT_BINARY


class Sandbox:
    """A private working directory, so config.toml is whatever the test says."""

    def __init__(self, config: Optional[str] = None):
        self.path = Path(tempfile.mkdtemp(prefix="avicoin-functional-"))
        if config is not None:
            (self.path / "config.toml").write_text(config)

    @property
    def data_dir(self) -> Path:
        """Where a node run from this sandbox keeps its chain.

        The node's own default is under the home directory, which every node on
        this machine shares -- including the developer's. A test must never
        touch it, so every node launched here is pointed somewhere private.
        """
        return self.path / "datadir"

    def cleanup(self) -> None:
        shutil.rmtree(self.path, ignore_errors=True)


class Node:
    """A spawned node, with a background thread draining its stdout."""

    def __init__(self, *args: str, sandbox: Optional[Sandbox] = None):
        self.sandbox = sandbox if sandbox is not None else Sandbox()
        self._lines: List[str] = []
        self._lock = threading.Lock()

        if "--data-dir" not in args:
            args = (*args, "--data-dir", str(self.sandbox.data_dir))

        self.process = subprocess.Popen(
            [str(binary_path()), *args],
            cwd=self.sandbox.path,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
        )

        self._reader = threading.Thread(target=self._drain, daemon=True)
        self._reader.start()

    def _drain(self) -> None:
        for line in self.process.stdout:
            with self._lock:
                self._lines.append(line.rstrip("\n"))

    def said(self) -> List[str]:
        with self._lock:
            return list(self._lines)

    def line_containing(self, needle: str, patience: float = PATIENCE) -> str:
        """A deadline, not a per-line timeout: a node that keeps saying
        something else would otherwise reset the clock forever."""
        deadline = time.monotonic() + patience

        while time.monotonic() < deadline:
            for line in self.said():
                if needle in line:
                    return line
            if self.process.poll() is not None:
                break
            time.sleep(0.02)

        raise AssertionError(
            f"nothing containing {needle!r} within {patience}s; "
            f"the node said:\n" + "\n".join(self.said())
        )

    def listening_on(self) -> str:
        return self.line_containing("Listening on").rsplit(" ", 1)[1]

    def wait_for_exit(self, patience: float = PATIENCE) -> int:
        try:
            return self.process.wait(timeout=patience)
        except subprocess.TimeoutExpired:
            raise AssertionError(
                f"the node was still running after {patience}s; it said:\n"
                + "\n".join(self.said())
            )

    def stop(self, cleanup: bool = True) -> None:
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        if cleanup:
            self.sandbox.cleanup()


def start_and_fail(*args: str, sandbox: Optional[Sandbox] = None):
    """Run a node that is expected to refuse to start, and return its output."""
    owned = sandbox if sandbox is not None else Sandbox()

    try:
        finished = subprocess.run(
            [str(binary_path()), *args],
            cwd=owned.path,
            capture_output=True,
            text=True,
            timeout=IMPATIENCE,
        )
    except subprocess.TimeoutExpired:
        raise AssertionError(
            "the node was expected to fail at startup, but it is still running"
        ) from None
    finally:
        if sandbox is None:
            owned.cleanup()

    assert finished.returncode != 0, (
        f"expected a non-zero exit, got {finished.returncode}\n"
        f"stdout: {finished.stdout}\nstderr: {finished.stderr}"
    )
    return finished
