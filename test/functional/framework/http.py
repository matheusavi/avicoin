"""A bounded HTTP client for a node's API.

Deliberately hand-rolled over a socket, for the same reason
`framework/messages.py` is a second implementation of the wire format: a test
that drives the node through the node's own idea of HTTP cannot catch the node
being wrong about HTTP. This one speaks HTTP/1.1 with `Connection: close`, so
the body is "everything until the socket closes" and there is no chunked
decoding to get wrong.

Every wait is bounded — ADR-0014. A node that answers slowly fails a test; a
node that never answers must not hang the suite.
"""

import json
import socket
from typing import Optional, Tuple

from .p2p import PATIENCE


class Refused(AssertionError):
    """The port is not being served at all."""


def request(
    address: str,
    path: str,
    method: str = "GET",
    body: Optional[bytes] = None,
    patience: float = PATIENCE,
) -> Tuple[int, bytes]:
    """The status code and the raw body. Raises rather than hanging."""
    host, port = address.rsplit(":", 1)
    lines = [
        f"{method} {path} HTTP/1.1",
        f"Host: {host}:{port}",
        "Connection: close",
    ]
    if body is not None:
        lines.append(f"Content-Length: {len(body)}")
    head = ("\r\n".join(lines) + "\r\n\r\n").encode()

    try:
        connection = socket.create_connection((host, int(port)), timeout=patience)
    except (ConnectionRefusedError, OSError) as why:
        raise Refused(f"nothing is serving {address}: {why}") from None

    with connection:
        connection.settimeout(patience)
        connection.sendall(head + (body or b""))

        received = b""
        while b"\r\n\r\n" not in received or True:
            try:
                more = connection.recv(4096)
            except socket.timeout:
                raise AssertionError(
                    f"{address}{path} sent nothing within {patience}s"
                ) from None
            if not more:
                break
            received += more

    head, _, payload = received.partition(b"\r\n\r\n")
    status = head.split(b"\r\n", 1)[0].split(b" ")
    if len(status) < 2 or not status[1].isdigit():
        raise AssertionError(f"{address}{path} did not answer with HTTP: {received!r}")

    return int(status[1]), payload


def get_json(address: str, path: str, patience: float = PATIENCE):
    status, body = request(address, path, patience=patience)
    try:
        return status, json.loads(body)
    except json.JSONDecodeError:
        raise AssertionError(f"{address}{path} answered {status} with {body!r}") from None


def raw(address: str, request_bytes: bytes, patience: float = PATIENCE) -> bytes:
    """A request the client above would not send, so a malformed one can be."""
    host, port = address.rsplit(":", 1)
    try:
        connection = socket.create_connection((host, int(port)), timeout=patience)
    except (ConnectionRefusedError, OSError) as why:
        raise Refused(f"nothing is serving {address}: {why}") from None

    with connection:
        connection.settimeout(patience)
        connection.sendall(request_bytes)
        received = b""
        while True:
            try:
                more = connection.recv(4096)
            except socket.timeout:
                break
            if not more:
                break
            received += more

    return received
