"""A node's data directory, and the chain it belongs to.

A directory carries a stamp naming the network that built it. Pointing a node
at the wrong one has to end the process rather than merge two chains, and two
nodes with directories of their own have to be able to share a host.
"""

from framework.node import Node, Sandbox


def test_a_fresh_directory_is_created_and_stamped():
    sandbox = Sandbox()
    node = Node("--host-address", "127.0.0.1:0", sandbox=sandbox)
    try:
        node.line_containing("Listening on")
        stamp = (sandbox.data_dir / "network").read_text()
    finally:
        node.stop()

    assert stamp.splitlines()[0] == "main"


def test_a_node_restarted_against_its_own_directory_starts_normally():
    sandbox = Sandbox()
    first = Node("--host-address", "127.0.0.1:0", sandbox=sandbox)
    first.line_containing("Listening on")
    first.stop(cleanup=False)

    second = Node("--host-address", "127.0.0.1:0", sandbox=sandbox)
    try:
        second.line_containing("Listening on")
    finally:
        second.stop()


def test_a_directory_built_by_another_network_ends_the_process():
    sandbox = Sandbox()
    built_on_test = Node(
        "--host-address", "127.0.0.1:0", "--network", "test", sandbox=sandbox
    )
    built_on_test.line_containing("Listening on")
    built_on_test.stop(cleanup=False)

    confused = Node("--host-address", "127.0.0.1:0", sandbox=sandbox)
    try:
        code = confused.wait_for_exit()
        said = "\n".join(confused.said())
    finally:
        confused.stop()

    assert code != 0, "a node on the wrong chain's directory must not run"
    assert str(sandbox.data_dir) in said, said
    assert "test" in said and "main" in said, said


def test_two_nodes_with_their_own_directories_share_a_host():
    one, two = Sandbox(), Sandbox()
    first = Node("--host-address", "127.0.0.1:0", sandbox=one)
    second = Node("--host-address", "127.0.0.1:0", sandbox=two)
    try:
        first.listening_on()
        second.listening_on()

        assert (one.data_dir / "network").exists()
        assert (two.data_dir / "network").exists()
    finally:
        first.stop()
        second.stop()
