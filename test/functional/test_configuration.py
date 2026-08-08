"""Defaults -> config.toml -> CLI, each overriding the previous where it
supplies a value. CLAUDE.md's "Configuration resolution" is the authority."""

from framework.p2p import IMPATIENCE, accept_within, address_of, expect_dialled


def test_with_no_config_and_no_arguments_a_node_uses_the_documented_default(net):
    node = net.node()

    # The default port is fixed, so it may already be taken on this machine.
    # Either outcome names the address, which is what is under test.
    line = node.line_containing("127.0.0.1:34352")
    assert "Listening on" in line or "could not listen" in line, line


def test_a_config_file_supplies_the_listening_address(net):
    probe = net.listener()
    net.node(
        config=f'[server]\nhost_address = "127.0.0.1:0"\n'
        f'addresses_to_connect = ["{address_of(probe)}"]\n'
    )

    peer = net.track(expect_dialled(probe))
    assert peer.next_frame().command == "version"


def test_a_command_line_address_overrides_the_config_file(net):
    ignored = net.listener()
    chosen = net.listener()

    net.node(
        "--host-address",
        "127.0.0.1:0",
        "--addresses-to-connect",
        address_of(chosen),
        config=f'[server]\nhost_address = "127.0.0.1:0"\n'
        f'addresses_to_connect = ["{address_of(ignored)}"]\n',
    )

    peer = net.track(expect_dialled(chosen))
    assert peer.next_frame().command == "version"

    assert (
        accept_within(ignored, IMPATIENCE) is None
    ), "the config file's peer was overridden and must not be dialled"
