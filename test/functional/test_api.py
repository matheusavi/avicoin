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

import socket
import time

import pytest

from framework.genesis import allocation, genesis_hash, read_lines
from framework.http import PATIENCE, Refused, get_json, raw, request
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


def mined_past(api: str, height: int, window: float = 12.0):
    """The node's status once its chain is past `height`."""
    deadline = time.monotonic() + window

    while time.monotonic() < deadline:
        status, body = get_json(api, "/status")
        if status == 200 and body["height"] > height - 1:
            return body

    raise AssertionError(f"the node never mined past {height} within {window}s")


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


def test_a_malformed_request_is_a_400_with_a_reason_and_the_node_lives(net):
    node, api = a_node(net)

    answered = raw(api, b"this is not HTTP at all\r\n\r\n")

    assert answered.startswith(b"HTTP/1.1 400"), answered
    assert b"application/json" in answered, answered
    assert b'"error"' in answered, answered
    assert node.process.poll() is None, "a stranger's bad request is not fatal"
    assert get_json(api, "/status")[0] == 200, "and the node is still serving"


def test_a_request_that_never_ends_is_refused_rather_than_buffered(net):
    """The reason HTTP is hand-rolled. A client that sends no newline is
    asking the node to buffer until it dies; the cap answers instead."""
    node, api = a_node(net)
    host, port = api.rsplit(":", 1)

    connection = socket.create_connection((host, int(port)), timeout=PATIENCE)
    answered = b""
    try:
        connection.settimeout(PATIENCE)
        # The node answers and closes partway through, so the writes stop
        # being deliverable — which is the point.
        try:
            for _ in range(4):
                connection.sendall(b"A" * 65536)
        except (BrokenPipeError, ConnectionResetError):
            pass
        while True:
            try:
                more = connection.recv(4096)
            except (ConnectionResetError, socket.timeout):
                break
            if not more:
                break
            answered += more
    finally:
        connection.close()

    assert answered.startswith(b"HTTP/1.1 400"), answered
    assert b"request head is at most" in answered, answered
    assert node.process.poll() is None
    assert get_json(api, "/status")[0] == 200


def test_many_silent_connections_are_bounded_and_the_node_comes_back(net):
    """A stranger holding sockets open costs a fixed number of threads and a
    fixed queue, and past that a connection is answered and closed rather than
    kept. Under that load a request is refused, not hung — and once the
    stranger lets go, the API is serving again."""
    node, api = a_node(net)
    host, port = api.rsplit(":", 1)

    held = []
    try:
        for _ in range(60):
            try:
                held.append(socket.create_connection((host, int(port)), timeout=1.0))
            except OSError:
                break

        assert node.process.poll() is None, "the node is still alive under it"
        try:
            status = get_json(api, "/status")[0]
            assert status in (200, 503), status
        except Refused:
            pass  # answered and closed, which is the bound doing its work
    finally:
        for connection in held:
            connection.close()

    deadline = time.monotonic() + PATIENCE
    while time.monotonic() < deadline:
        try:
            if get_json(api, "/status")[0] == 200:
                return
        except Refused:
            pass

    raise AssertionError("the API never recovered once the sockets were released")


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


def test_a_block_is_served_by_hash_and_by_height(net):
    node, api = a_node(net, "--mine")
    tip = mined_past(api, 1)

    by_height = get_json(api, f"/block/height/{tip['height']}")[1]
    by_hash = get_json(api, f"/block/{tip['tip']}")[1]

    assert by_hash == by_height
    assert by_hash["hash"] == tip["tip"]
    assert by_hash["transactions"][0]["coinbase"] is True


def test_a_transaction_is_served_with_both_of_its_ids(net):
    _, api = a_node(net, "--mine")
    tip = mined_past(api, 1)
    coinbase = get_json(api, f"/block/{tip['tip']}")[1]["transactions"][0]

    status, body = get_json(api, f"/tx/{coinbase['txid']}")

    assert status == 200
    assert body["txid"] == coinbase["txid"]
    assert body["wtxid"] == coinbase["wtxid"]
    assert body["height"] == tip["height"]


def test_a_hash_a_response_gives_is_one_the_api_takes_back(net):
    """The round trip is what pins the byte order: a hash the API prints has
    to be a hash the API accepts, or the reversal happens in one direction
    only and nobody notices until an explorer does."""
    _, api = a_node(net, "--mine")
    tip = mined_past(api, 1)

    assert get_json(api, f"/block/{tip['tip']}")[0] == 200


def test_a_page_of_blocks_is_capped(net):
    _, api = a_node(net, "--mine")
    mined_past(api, 1)

    assert get_json(api, "/blocks?count=1")[1]["blocks"].__len__() == 1
    assert get_json(api, "/blocks?count=100000")[0] == 400


def test_asking_for_what_is_not_there(net):
    _, api = a_node(net)

    assert get_json(api, "/block/" + "11" * 32)[0] == 404
    assert get_json(api, "/block/height/9999")[0] == 404
    assert get_json(api, "/tx/" + "22" * 32)[0] == 404
    assert get_json(api, "/block/not-a-hash")[0] == 400
    assert get_json(api, "/block/height/seven")[0] == 400


def test_an_address_holds_what_the_allocation_gave_it(net):
    """The allocation is the one balance a test can know independently: it is
    a checked-in file, and `framework/genesis.py` reads it without asking the
    node."""
    _, api = a_node(net)
    allocated = allocation()

    for address_line, atoms in zip(read_lines("testnet.allocation"), allocated):
        address = address_line.split()[0]
        status, body = get_json(api, f"/address/{address}")

        assert status == 200, body
        assert body["atoms"] == atoms[1], body
        assert len(body["unspent"]) == 1, body
        assert body["unspent"][0]["coinbase"] is True, body


def test_the_mempool_and_the_peers_and_the_log_are_served(net):
    node, api = a_node(net)

    mempool = get_json(api, "/mempool")[1]
    peers = get_json(api, "/peers")[1]
    log = get_json(api, "/log")[1]

    assert mempool == {"count": 0, "transactions": []}
    assert peers == {"count": 0, "peers": []}
    assert any("Listening on" in line for line in log["lines"]), log
    assert get_json(api, f"/log?since={log['next']}")[1]["lines"] == []


def test_a_peer_is_reported_by_where_it_listens(net):
    first, api = a_node(net)
    second, _ = a_node(net, "--addresses-to-connect", first.listening_on())
    listening = second.listening_on()

    deadline = time.monotonic() + 8.0
    while time.monotonic() < deadline:
        peers = get_json(api, "/peers")[1]
        if peers["count"] and peers["peers"][0]["handshake"] == "ready":
            break

    assert peers["count"] == 1, peers
    assert peers["peers"][0]["listening"] == listening, peers
    assert peers["peers"][0]["direction"] == "inbound", peers


def test_an_address_that_is_not_an_address_is_a_400(net):
    _, api = a_node(net)

    assert get_json(api, "/address/not-an-address")[0] == 400
    assert get_json(api, "/address/1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN2")[0] == 400
