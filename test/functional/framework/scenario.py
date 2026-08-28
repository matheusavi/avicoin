"""Several nodes at once, and the things a scenario needs to do to them.

A scenario describes a *network* rather than a sequence of process launches:
how many nodes, which of them mine, and who has been told about whom. It then
waits on **states** — "these two agree on a tip", "this one reached height N" —
never on durations, and every wait carries its own deadline (ADR-0014).

Observation is the API. With M6 shipped there is a real surface, so nothing
here reads stdout except to learn the port a node bound and that its API is up
— which is how a scenario finds the thing it then asks about.

**A partition is a network that was never introduced, and healing is
one-way.** Two nodes that have never heard of each other are apart; `introduce`
makes one dial the other through `POST /connect`, the same path a configured
peer takes. There is no `separate`: once they are peers, discovery has already
told everyone else where they listen, and pretending otherwise would be a
scenario that lies. Splitting a network that was whole needs control of the
sockets, which is a different tool.
"""

import time
from typing import Callable, List, Optional

from .http import Refused, get_json, request
from .node import Node
from .p2p import a_free_address

# Longer than PATIENCE: a scenario waits for several processes to mine, relay
# and connect a block, and runs them on whatever core CI gave it. Spent only
# where a state genuinely takes several blocks to reach.
SETTLE = 45.0

# How often a wait looks again. Spinning takes workers from the very node the
# scenario is waiting on — `test_api`'s loops learned that the hard way.
GLANCE = 0.05


class Runner:
    """One node in a scenario, and its API."""

    def __init__(self, node: Node, api: str, name: str):
        self.node = node
        self.api = api
        self.name = name

    def status(self) -> dict:
        return get_json(self.api, "/status")[1]

    def height(self) -> int:
        return self.status()["height"]

    def tip(self) -> str:
        return self.status()["tip"]

    def peers(self) -> list:
        return get_json(self.api, "/peers")[1]["peers"]

    def block_at(self, height: int) -> Optional[dict]:
        status, body = get_json(self.api, f"/block/height/{height}")
        return body if status == 200 else None

    def kill(self) -> None:
        self.node.kill()

    def stop(self) -> None:
        self.node.stop(cleanup=False)

    def __repr__(self) -> str:
        return f"{self.name}@{self.api}"


class Scenario:
    """N nodes on the test network, each with its own directory and ports."""

    def __init__(self, net):
        self.net = net
        self.runners: List[Runner] = []

    def start(self, name: str, mining: bool = False, knows: Optional[List[Runner]] = None) -> Runner:
        api = a_free_address()
        dials = [
            part
            for peer in (knows or [])
            for part in ("--addresses-to-connect", peer.node.listening_on())
        ]
        node = self.net.node(
            "--network",
            "test",
            "--host-address",
            "127.0.0.1:0",
            "--api-address",
            api,
            *(["--mine"] if mining else []),
            *dials,
        )
        node.line_containing("API on")

        runner = Runner(node, api, name)
        self.runners.append(runner)
        return runner

    def restart(self, runner: Runner, mining: bool = False, knows: Optional[List[Runner]] = None):
        """The same data directory, a new process. What a scenario killed has
        to be able to come back, or the crash it staged proves nothing.

        Stops it first if it is still running — a node holds an advisory lock
        on its directory, so a second one would refuse to start rather than
        take over. A no-op on a process already killed.
        """
        runner.node.stop(cleanup=False)
        dials = [
            part
            for peer in (knows or [])
            for part in ("--addresses-to-connect", peer.node.listening_on())
        ]
        started = self.net.reuse(
            runner.node,
            "--network",
            "test",
            "--host-address",
            "127.0.0.1:0",
            "--api-address",
            runner.api,
            *(["--mine"] if mining else []),
            *dials,
        )
        started.line_containing("API on")
        runner.node = started
        return runner

    def introduce(self, one: Runner, other: Runner) -> None:
        """Heal, through the same dial a configured peer uses. One-way by
        design: see this module's docstring."""
        status, body = request(
            one.api, "/connect", method="POST", body=other.node.listening_on().encode()
        )
        assert status == 200, f"{one} could not be told about {other}: {body!r}"


def until(condition: Callable[[], bool], window: float = SETTLE, what: str = "") -> None:
    """A state, with a deadline. Never a duration."""
    deadline = time.monotonic() + window

    while time.monotonic() < deadline:
        try:
            if condition():
                return
        except (Refused, AssertionError):
            # A node that is not answering yet is not a failure yet; the
            # deadline is what decides.
            pass
        time.sleep(GLANCE)

    raise AssertionError(f"{what or 'the condition'} did not hold within {window}s")


def agreed(runners: List[Runner], beyond: int = 0) -> Optional[str]:
    """The tip they share once they share one past `beyond`, or None.

    Height is not enough: two nodes at the same height on different branches
    have not agreed on anything.
    """
    tips = set()
    for runner in runners:
        status = runner.status()
        if status["height"] <= beyond:
            return None
        tips.add(status["tip"])

    return tips.pop() if len(tips) == 1 else None
