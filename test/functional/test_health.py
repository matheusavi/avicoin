"""`avicoin health` — the container's healthcheck.

Up is not the same as working. A node whose miner has wedged answers
`/status` perfectly well and is doing nothing, so the question this asks is
whether the **tip has moved** since it last looked.
"""

import subprocess
import time
from pathlib import Path

from framework.http import get_json
from framework.node import binary_path
from framework.p2p import PATIENCE, a_free_address


def a_node(net, *args: str):
    api = a_free_address()
    node = net.node(
        "--network", "test", "--host-address", "127.0.0.1:0", "--api-address", api, *args
    )
    node.line_containing("API on")
    return node, api


def checked(node, api, marker: Path, *args: str):
    return subprocess.run(
        [
            str(binary_path()),
            "health",
            "--api-address",
            api,
            "--marker",
            str(marker),
            *args,
        ],
        capture_output=True,
        text=True,
        timeout=PATIENCE,
        cwd=node.sandbox.path,
    )


def test_a_mining_node_is_healthy_and_stays_healthy(net):
    node, api = a_node(net, "--mine")
    marker = node.sandbox.path / "health"

    assert checked(node, api, marker).returncode == 0, "a first look cannot tell"
    assert marker.exists()

    # Long enough for the test network, which wants a block a second.
    deadline = time.monotonic() + PATIENCE
    while time.monotonic() < deadline:
        if get_json(api, "/status")[1]["height"] > 1:
            break
        time.sleep(0.05)

    assert checked(node, api, marker).returncode == 0, "the tip moved"


def test_a_node_whose_tip_stands_still_is_unhealthy(net):
    """Not mining and with no peers, so its tip cannot move — which is exactly
    the shape of a wedged node that is still answering."""
    node, api = a_node(net)
    marker = node.sandbox.path / "health"

    assert checked(node, api, marker).returncode == 0, "a first look cannot tell"
    # `now()` is whole seconds, so a stall of zero needs one boundary crossed.
    time.sleep(1.5)
    stalled = checked(node, api, marker, "--stall-seconds", "0")

    assert stalled.returncode != 0
    assert "stood at" in stalled.stderr, stalled.stderr


def test_a_node_that_is_not_answering_is_unhealthy(net):
    node, api = a_node(net)
    marker = node.sandbox.path / "health"
    node.stop(cleanup=False)

    assert checked(node, api, marker).returncode != 0
