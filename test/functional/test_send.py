"""`avicoin send` — the only way to spend, and the reason there is no
`POST /send`.

The key stays on the machine that holds it. What crosses to the node is a
signed transaction, which is the same thing any stranger could have sent it —
so a public URL never carries spending authority.

Every wait is bounded and observation is the API, not stdout, except for the
subcommand's own output: it is a separate process whose *whole* result is what
it prints.
"""

import subprocess
import time

from framework.http import get_json
from framework.node import Sandbox, binary_path
from framework.p2p import PATIENCE, a_free_address


def a_node(net, *args: str):
    api = a_free_address()
    node = net.node(
        "--network", "test", "--host-address", "127.0.0.1:0", "--api-address", api, *args
    )
    node.line_containing("API on")
    return node, api


def sent(node, api, *args: str):
    return subprocess.run(
        [
            str(binary_path()),
            "send",
            "--api-address",
            api,
            "--data-dir",
            str(node.sandbox.data_dir),
            *args,
        ],
        capture_output=True,
        text=True,
        timeout=PATIENCE,
        cwd=node.sandbox.path,
    )


def mined_past(api: str, height: int, window: float = 20.0):
    deadline = time.monotonic() + window

    while time.monotonic() < deadline:
        status, body = get_json(api, "/status")
        if status == 200 and body["height"] > height:
            return body
        time.sleep(0.05)

    raise AssertionError(f"never mined past {height} within {window}s")


# The test network matures a coinbase in one block, so the first block's
# reward is spendable almost at once.
SPENDABLE = 3


def confirmed_within(api: str, txid: str, window: float = 20.0):
    """The transaction once a block holds it.

    `GET /tx` answers from the mempool first, so "it is served" is not "it was
    mined" — the block field is what tells them apart, and asserting the former
    while claiming the latter is a test that passes against a miner which never
    includes anything."""
    deadline = time.monotonic() + window

    while time.monotonic() < deadline:
        status, body = get_json(api, f"/tx/{txid}")
        if status == 200 and body.get("block"):
            return body
        time.sleep(0.05)

    raise AssertionError(f"{txid} was never mined within {window}s")


def test_a_send_reaches_the_mempool_and_then_a_block(net):
    node, api = a_node(net, "--mine")
    mined_past(api, SPENDABLE)
    to = an_address(api)

    finished = sent(node, api, "--to", to, "--amount", "1")

    assert finished.returncode == 0, finished.stderr
    txid = finished.stdout.strip()
    assert len(txid) == 64, finished.stdout

    held = get_json(api, "/mempool")[1]
    assert any(t["txid"] == txid for t in held["transactions"]), held

    mined = confirmed_within(api, txid)
    assert mined["txid"] == txid
    assert mined["height"] >= 1

    # The block really carries it, and the money really moved.
    block = get_json(api, f"/block/{mined['block']}")[1]
    assert any(t["txid"] == txid for t in block["transactions"]), block
    assert get_json(api, f"/address/{to}")[1]["atoms"] > 0, "the payee was paid"


def an_address(api: str) -> str:
    """Somewhere to pay that is not the sender: the genesis allocation's first
    address, which the suite knows independently."""
    from framework.genesis import read_lines

    return read_lines("testnet.allocation")[0].split()[0]


def test_a_send_of_more_than_is_held_says_how_much_is_short(net):
    node, api = a_node(net, "--mine")
    mined_past(api, SPENDABLE)

    finished = sent(node, api, "--to", an_address(api), "--amount", "1000000")

    assert finished.returncode != 0
    assert "short by" in finished.stderr, finished.stderr
    assert "can spend" in finished.stderr, finished.stderr
    assert get_json(api, "/mempool")[1]["count"] == 0, "and nothing was signed"


def test_a_send_to_something_that_is_not_an_address_is_refused(net):
    node, api = a_node(net, "--mine")
    mined_past(api, SPENDABLE)

    finished = sent(node, api, "--to", "not-an-address", "--amount", "1")

    assert finished.returncode != 0
    assert get_json(api, "/mempool")[1]["count"] == 0


def test_a_send_from_a_directory_with_no_key_mints_nothing(net):
    _, api = a_node(net)
    empty = Sandbox()

    finished = subprocess.run(
        [
            str(binary_path()),
            "send",
            "--api-address",
            api,
            "--data-dir",
            str(empty.data_dir),
            "--to",
            an_address(api),
            "--amount",
            "1",
        ],
        capture_output=True,
        text=True,
        timeout=PATIENCE,
    )

    try:
        assert finished.returncode != 0
        assert "holds no wallet key" in finished.stderr, finished.stderr
        assert not (empty.data_dir / "wallet.key").exists(), "nothing was minted"
    finally:
        empty.cleanup()


def test_a_send_with_no_node_answering_says_so(net):
    node, api = a_node(net)
    nowhere = a_free_address()

    finished = sent(node, nowhere, "--to", an_address(api), "--amount", "1")

    assert finished.returncode != 0
    assert nowhere in finished.stderr, finished.stderr
