"""One handle for everything a test starts, so nothing is left running."""

import socket
from typing import List, Optional

from .node import Node, Sandbox
from .messages import MAGIC
from .p2p import Peer, address_of, free_port, listen_on


class Network:
    def __init__(self):
        self._nodes: List[Node] = []
        self._listeners: List[socket.socket] = []
        self._peers: List[Peer] = []

    def node(self, *args: str, config: Optional[str] = None) -> Node:
        started = Node(*args, sandbox=Sandbox(config))
        self._nodes.append(started)
        return started

    def restart(self, node: Node, *args: str) -> Node:
        """The same data directory, a new process, so the tests that matter
        here — did it come back where it was — have something to compare."""
        node.stop(cleanup=False)
        return self.reuse(node, *args)

    def reuse(self, node: Node, *args: str) -> Node:
        """A new process on a stopped node's directory. Its sandbox is handed
        over rather than copied, so only the last node owns the cleanup."""
        started = Node(*args, sandbox=node.sandbox)
        self._nodes.append(started)
        return started

    def listener(self) -> socket.socket:
        """A socket a node can be pointed at, so we see it dial out."""
        listening = free_port()
        self._listeners.append(listening)
        return listening

    def listener_on(self, address: str) -> socket.socket:
        """The same, on an address a node has already been told to dial."""
        listening = listen_on(address)
        self._listeners.append(listening)
        return listening

    def address(self) -> str:
        return address_of(self.listener())

    def dial(self, address: str, magic: bytes = MAGIC) -> Peer:
        peer = Peer.dial(address, magic)
        self._peers.append(peer)
        return peer

    def track(self, peer: Peer) -> Peer:
        self._peers.append(peer)
        return peer

    def cleanup(self) -> None:
        for peer in self._peers:
            peer.close()
        for listening in self._listeners:
            listening.close()
        # Several nodes can share one sandbox across a restart, so stopping
        # them in reverse leaves the directory's removal to the last starter.
        for started in reversed(self._nodes):
            started.stop(cleanup=False)
        for sandbox in {id(n.sandbox): n.sandbox for n in self._nodes}.values():
            sandbox.cleanup()
