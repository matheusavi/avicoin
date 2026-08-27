"""The wallet's key on disk.

The key is the node's identity as a miner: without it on disk, every restart
mines to a new address and yesterday's coins belong to nobody.
"""

import os
import stat

from framework.node import Node, Sandbox, start_and_fail

KEY_FILE = "wallet.key"


def said_by(finished):
    return finished.stdout + finished.stderr


def a_node(sandbox):
    return Node("--host-address", "127.0.0.1:0", sandbox=sandbox)


def a_started_node(sandbox):
    """Started once and stopped, leaving its directory for the next one."""
    node = a_node(sandbox)
    try:
        node.line_containing("Listening on")
    finally:
        node.stop(cleanup=False)


def test_a_key_is_written_on_the_first_run_and_kept_afterwards():
    with Sandbox() as sandbox:
        a_started_node(sandbox)
        written = (sandbox.data_dir / KEY_FILE).read_text()

        second = a_node(sandbox)
        try:
            second.line_containing("Listening on")
            assert (sandbox.data_dir / KEY_FILE).read_text() == written
        finally:
            second.stop(cleanup=False)

        assert len(written.strip()) == 64, written


def test_a_key_is_written_readable_by_nobody_else():
    with Sandbox() as sandbox:
        a_started_node(sandbox)

        mode = stat.S_IMODE(os.stat(sandbox.data_dir / KEY_FILE).st_mode)

        assert mode == 0o600, oct(mode)


def test_a_key_anyone_can_read_ends_the_process():
    with Sandbox() as sandbox:
        a_started_node(sandbox)
        os.chmod(sandbox.data_dir / KEY_FILE, 0o644)

        said = said_by(start_and_fail("--host-address", "127.0.0.1:0", sandbox=sandbox))

        assert KEY_FILE in said, said
        assert "644" in said, said


def test_a_key_file_that_is_not_a_key_ends_the_process():
    with Sandbox() as sandbox:
        a_started_node(sandbox)
        (sandbox.data_dir / KEY_FILE).write_text("not a private key\n")
        os.chmod(sandbox.data_dir / KEY_FILE, 0o600)

        said = said_by(start_and_fail("--host-address", "127.0.0.1:0", sandbox=sandbox))

        assert KEY_FILE in said, said


def test_a_data_directory_anyone_can_write_to_ends_the_process():
    with Sandbox() as sandbox:
        a_started_node(sandbox)
        os.chmod(sandbox.data_dir, 0o777)

        said = said_by(start_and_fail("--host-address", "127.0.0.1:0", sandbox=sandbox))

        assert "777" in said, said


def test_two_nodes_have_two_keys():
    with Sandbox() as one, Sandbox() as two:
        first, second = a_node(one), a_node(two)
        try:
            first.listening_on()
            second.listening_on()

            assert (one.data_dir / KEY_FILE).read_text() != (
                two.data_dir / KEY_FILE
            ).read_text()
        finally:
            first.stop(cleanup=False)
            second.stop(cleanup=False)
