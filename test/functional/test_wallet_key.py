"""The wallet's key on disk.

The key is the node's identity as a miner: without it on disk, every restart
mines to a new address and yesterday's coins belong to nobody.
"""

import os
import stat

from framework.node import Node, Sandbox

KEY_FILE = "wallet.key"


def a_node(sandbox):
    return Node("--host-address", "127.0.0.1:0", sandbox=sandbox)


def test_a_key_is_written_on_the_first_run_and_kept_afterwards():
    sandbox = Sandbox()
    first = a_node(sandbox)
    try:
        first.line_containing("Listening on")
        written = (sandbox.data_dir / KEY_FILE).read_text()
    finally:
        first.stop(cleanup=False)

    second = a_node(sandbox)
    try:
        second.line_containing("Listening on")

        assert (sandbox.data_dir / KEY_FILE).read_text() == written
    finally:
        second.stop()

    assert len(written.strip()) == 64, written


def test_a_key_is_written_readable_by_nobody_else():
    sandbox = Sandbox()
    node = a_node(sandbox)
    try:
        node.line_containing("Listening on")
        mode = stat.S_IMODE(os.stat(sandbox.data_dir / KEY_FILE).st_mode)
    finally:
        node.stop()

    assert mode == 0o600, oct(mode)


def test_a_key_anyone_can_read_ends_the_process():
    sandbox = Sandbox()
    first = a_node(sandbox)
    try:
        first.line_containing("Listening on")
    finally:
        first.stop(cleanup=False)

    os.chmod(sandbox.data_dir / KEY_FILE, 0o644)

    widened = a_node(sandbox)
    try:
        code = widened.wait_for_exit()
        said = "\n".join(widened.said())
    finally:
        widened.stop()

    assert code != 0, said
    assert KEY_FILE in said, said


def test_a_key_file_that_is_not_a_key_ends_the_process():
    sandbox = Sandbox()
    first = a_node(sandbox)
    try:
        first.line_containing("Listening on")
    finally:
        first.stop(cleanup=False)

    (sandbox.data_dir / KEY_FILE).write_text("not a private key\n")
    os.chmod(sandbox.data_dir / KEY_FILE, 0o600)

    confused = a_node(sandbox)
    try:
        assert confused.wait_for_exit() != 0
        assert KEY_FILE in "\n".join(confused.said())
    finally:
        confused.stop()


def test_two_nodes_have_two_keys():
    one, two = Sandbox(), Sandbox()
    first, second = a_node(one), a_node(two)
    try:
        first.listening_on()
        second.listening_on()

        assert (one.data_dir / KEY_FILE).read_text() != (
            two.data_dir / KEY_FILE
        ).read_text()
    finally:
        first.stop()
        second.stop()
