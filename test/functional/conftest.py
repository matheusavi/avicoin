import pytest

from framework.network import Network


@pytest.fixture
def net():
    network = Network()
    yield network
    network.cleanup()
