"""A test-side peer: dials or accepts a connection and speaks the protocol.

Every wait here is bounded. A hanging test is worse than a failing one, because
it takes the suite with it rather than reporting one red case.
"""

import socket
import time
from typing import List, Optional

from .messages import Frame, parse, verack, version

# How long to wait for something that should happen. Every operation it guards
# -- a process exec, a loopback connect, a ping already queued -- is sub-second
# in practice, so this is headroom for a loaded CI runner, not a real duration.
# It is also the price of every failure: pytest runs serially, so a broken node
# costs this much per test. Raise it only with that multiplication in mind.
PATIENCE = 8.0
# How long to wait before concluding something will *not* happen. Paid on every
# passing run, so it dominates the suite's runtime rather than its failures.
IMPATIENCE = 3.0


class Peer:
    def __init__(self, sock: socket.socket):
        self.socket = sock
        self.socket.settimeout(PATIENCE)
        self.buffer = b""

    @classmethod
    def dial(cls, address: str) -> "Peer":
        return cls(socket.create_connection(split_address(address), timeout=PATIENCE))

    def send(self, payload: bytes) -> None:
        self.socket.sendall(payload)

    def _take_frame(self) -> Optional[Frame]:
        parsed, consumed = parse(self.buffer)
        if parsed is None:
            return None
        self.buffer = self.buffer[consumed:]
        return parsed

    def next_frame(self) -> Frame:
        while True:
            parsed = self._take_frame()
            if parsed is not None:
                return parsed

            try:
                received = self.socket.recv(4096)
            except socket.timeout:
                raise AssertionError(
                    f"the node sent nothing within {PATIENCE}s"
                ) from None
            assert received, "the node closed the connection unexpectedly"
            self.buffer += received

    def next_frame_of(self, command: str) -> Frame:
        """The next frame of one kind, past whatever else the node is saying.

        A deadline around the loop, not just inside `next_frame`: a node that
        keeps sending something else would otherwise reset the clock forever.
        """
        deadline = time.monotonic() + PATIENCE

        while time.monotonic() < deadline:
            received = self.next_frame()
            if received.command == command:
                return received

        raise AssertionError(f"the node sent no {command} within {PATIENCE}s")

    def handshake(self, listen_address: str = "127.0.0.1:5000", nonce: int = 0x51DE) -> None:
        """Become a peer: answer the node's version, and send our own."""
        self.next_frame_of("version")
        self.send(version(nonce, listen_address))
        self.next_frame_of("verack")
        self.send(verack())

    def frames_within(self, window: float = IMPATIENCE) -> List[Frame]:
        """Everything the node says within `window`.

        Bounded, so a node that goes quiet -- or that only ever repeats its
        ping -- fails rather than hangs.
        """
        deadline = time.monotonic() + window
        frames: List[Frame] = []

        while True:
            while True:
                parsed = self._take_frame()
                if parsed is None:
                    break
                frames.append(parsed)

            left = deadline - time.monotonic()
            if left <= 0:
                return frames

            self.socket.settimeout(left)
            try:
                received = self.socket.recv(4096)
            except socket.timeout:
                return frames
            except OSError:
                return frames

            if not received:
                return frames
            self.buffer += received

    def pongs_within(self, window: float = IMPATIENCE) -> List[int]:
        return [frame.nonce for frame in self.frames_within(window) if frame.command == "pong"]

    def expect_closed(self) -> None:
        deadline = time.monotonic() + IMPATIENCE

        while time.monotonic() < deadline:
            self.socket.settimeout(max(deadline - time.monotonic(), 0.01))
            try:
                if not self.socket.recv(4096):
                    return
            except socket.timeout:
                break
            except OSError:
                return

        raise AssertionError(
            "the node kept the connection open after a message it should have refused"
        )

    def expect_silence(self) -> None:
        self.socket.settimeout(IMPATIENCE)
        try:
            received = self.socket.recv(4096)
        except socket.timeout:
            return
        except OSError:
            return

        assert not received, f"expected silence, got {len(received)} more bytes"

    def close(self) -> None:
        try:
            self.socket.close()
        except OSError:
            pass


def split_address(address: str):
    host, port = address.rsplit(":", 1)
    return host, int(port)


def free_port() -> socket.socket:
    """A listening socket on an ephemeral port, kept open so nothing races us."""
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", 0))
    listener.listen(8)
    return listener


def address_of(listener: socket.socket) -> str:
    host, port = listener.getsockname()
    return f"{host}:{port}"


def accept_within(listener: socket.socket, patience: float) -> Optional[socket.socket]:
    """`accept` blocks forever, which turns "the node never dialled" into a hang."""
    listener.settimeout(patience)
    try:
        accepted, _ = listener.accept()
    except socket.timeout:
        return None
    accepted.settimeout(PATIENCE)
    return accepted


def expect_dialled(listener: socket.socket) -> Peer:
    accepted = accept_within(listener, PATIENCE)
    assert accepted is not None, f"the node never dialled us within {PATIENCE}s"
    return Peer(accepted)
