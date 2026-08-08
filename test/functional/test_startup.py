"""A bad start fails the process. It never produces a limping node.

main.rs binds the listener before spawning anything, precisely so a bad address
or a taken port kills the process rather than a detached thread.
"""

from framework.node import Sandbox, start_and_fail
from framework.p2p import address_of


def test_an_address_that_cannot_be_bound_fails_the_process(net):
    occupied = net.listener()

    failed = start_and_fail("--host-address", address_of(occupied))

    assert address_of(occupied) in failed.stderr, (
        "the failure should name the address it could not bind, "
        f"got: {failed.stderr}"
    )


def test_a_malformed_address_in_the_config_file_fails_the_process():
    sandbox = Sandbox('[server]\nhost_address = "not-an-address"\n')
    try:
        failed = start_and_fail(sandbox=sandbox)
    finally:
        sandbox.cleanup()

    assert "host_address" in failed.stderr and "not-an-address" in failed.stderr, (
        f"the failure should name the field and the value, got: {failed.stderr}"
    )


def test_an_unknown_key_in_the_config_file_fails_the_process():
    sandbox = Sandbox('[server]\nhost_adress = "127.0.0.1:1"\n')
    try:
        start_and_fail(sandbox=sandbox)
    finally:
        sandbox.cleanup()


def test_an_unparseable_config_file_fails_the_process():
    sandbox = Sandbox("this is not toml at all\n")
    try:
        start_and_fail(sandbox=sandbox)
    finally:
        sandbox.cleanup()


def test_an_unreachable_peer_is_logged_and_the_node_keeps_listening(net):
    closed = net.listener()
    unreachable = address_of(closed)
    closed.close()

    node = net.node(
        "--host-address", "127.0.0.1:0", "--addresses-to-connect", unreachable
    )
    address = node.listening_on()

    node.line_containing("Could not connect")
    assert net.dial(address).next_frame().command == "version"
