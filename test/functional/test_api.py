"""The HTTP API, driven over a real socket against a real binary.

The client in `framework/http.py` is hand-rolled for the same reason
`framework/messages.py` is: a test that used the node's own HTTP would not
catch the node being wrong about HTTP.

Off by default is the guarantee that matters most here — exposing a node to
HTTP has to be a decision somebody makes. It is pinned in two halves, because
neither half is enough alone: `config.rs` has the unit test that the resolved
default is `None`, and the paired test below shows an address that answered
with the flag going silent without it. A test that merely found some unused
port refused would prove nothing, since no port was going to answer.
"""

import pytest

from framework.genesis import genesis_hash
from framework.http import Refused, get_json, raw, request
from framework.node import Sandbox, start_and_fail
from framework.p2p import a_free_address


def a_node(net, *args: str):
    """Serving its API on an address the test picked, since `:0` would leave
    us nothing to connect to."""
    api = a_free_address()
    node = net.node(
        "--network", "test", "--host-address", "127.0.0.1:0", "--api-address", api, *args
    )
    node.line_containing("API on")
    return node, api


def test_a_node_without_an_api_address_serves_nothing(net):
    """The same address, twice: once with the flag and once without.

    Asserting that some unused port is refused would prove nothing — no port
    was going to answer. Only the pair shows that the silence is caused by the
    absence of the flag.
    """
    api = a_free_address()
    served = net.node(
        "--network", "test", "--host-address", "127.0.0.1:0", "--api-address", api
    )
    served.line_containing("API on")
    assert get_json(api, "/status")[0] == 200
    served.stop(cleanup=False)

    silent = net.reuse(served, "--network", "test", "--host-address", "127.0.0.1:0")
    silent.listening_on()

    with pytest.raises(Refused):
        get_json(api, "/status")


def test_status_reports_the_tip_and_the_network(net):
    _, api = a_node(net)

    status, body = get_json(api, "/status")

    assert status == 200
    assert body["network"] == "test"
    assert body["height"] == 0
    assert body["tip"] == genesis_hash()[::-1].hex()
    assert body["peers"] == 0
    assert body["mempool"] == 0


def test_an_unknown_path_is_a_404_with_a_reason(net):
    _, api = a_node(net)

    status, body = get_json(api, "/there-is-no-such-thing")

    assert status == 404
    assert isinstance(body["error"], str)


def test_a_malformed_request_does_not_kill_the_node(net):
    node, api = a_node(net)

    raw(api, b"this is not HTTP at all\r\n\r\n")

    assert node.process.poll() is None, "a stranger's bad request is not fatal"
    assert get_json(api, "/status")[0] == 200, "and the node is still serving"


def test_an_api_address_that_cannot_be_bound_fails_the_process(net):
    _, api = a_node(net)

    said = start_and_fail(
        "--network", "test", "--host-address", "127.0.0.1:0", "--api-address", api
    )

    assert api in said.stdout + said.stderr


def test_an_unparseable_api_address_fails_the_process():
    with Sandbox() as sandbox:
        said = start_and_fail(
            "--host-address", "127.0.0.1:0", "--api-address", "8080", sandbox=sandbox
        )

    assert "api_address" in said.stdout + said.stderr


def test_several_requests_at_once_are_all_answered(net):
    """The pool is fixed, so more clients than workers must queue rather than
    be dropped."""
    _, api = a_node(net)

    for _ in range(12):
        assert request(api, "/status")[0] == 200
