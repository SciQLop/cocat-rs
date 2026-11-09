"""The short form: `cc.login()`, `cc.open()` and the `Client` behind them, against the server
binary from `conftest.py`, with an in-memory keyring and a "browser" that answers the page."""

import socket
import sqlite3
import urllib.parse
import urllib.request

import keyring.errors
import pytest

import cocat_rs as cc
from cocat_rs import client as client_module

from conftest import PASSWORD

WALLET: dict[tuple[str, str], str] = {}


@pytest.fixture(autouse=True)
def wallet(monkeypatch):
    WALLET.clear()
    monkeypatch.setattr(
        client_module.keyring, "get_password", lambda service, key: WALLET.get((service, key))
    )
    monkeypatch.setattr(
        client_module.keyring,
        "set_password",
        lambda service, key, value: WALLET.__setitem__((service, key), value),
    )

    def delete(service, key):
        if WALLET.pop((service, key), None) is None:
            raise keyring.errors.PasswordDeleteError("not there")

    monkeypatch.setattr(client_module.keyring, "delete_password", delete)
    monkeypatch.setattr(client_module, "_MEMORY", {})
    monkeypatch.setattr(client_module, "_DEFAULT", None)
    monkeypatch.delenv("COCAT_SERVER", raising=False)
    yield WALLET


def decide(server, code, decision="allow"):
    """What the person does on the connect page, signed in with the fixture's cookie."""
    data = urllib.parse.urlencode({"code": code, "decision": decision}).encode()
    request = urllib.request.Request(f"{server.url}/ui/connect", data=data, method="POST")
    request.add_header("cookie", f"fastapiusersauth={server.cookie}")
    urllib.request.urlopen(request).close()


def connect_page(server, code):
    request = urllib.request.Request(f"{server.url}/ui/connect?code={code}")
    request.add_header("cookie", f"fastapiusersauth={server.cookie}")
    with urllib.request.urlopen(request) as response:
        return response.read().decode()


@pytest.fixture
def browser(server, monkeypatch):
    """A desktop whose browser allows every sign-in link it is given."""
    opened = []

    def open_link(url):
        opened.append(url)
        code = urllib.parse.parse_qs(urllib.parse.urlparse(url).query)["code"][0]
        decide(server, code)
        return True

    monkeypatch.setattr(client_module.sys, "platform", "linux")
    monkeypatch.setenv("DISPLAY", ":0")
    monkeypatch.setattr(client_module.webbrowser, "open", open_link)
    return opened


@pytest.fixture
def signed_in(server, browser):
    client = cc.Client(server.url)
    client.login()
    return client


def bench_names(client):
    return [project.name for project in client.projects()]


def test_login_opens_the_link_and_keeps_the_token_in_the_keyring(server, browser, capsys):
    client = cc.Client(server.url)

    client.login()

    printed = capsys.readouterr().out
    assert browser[0] in printed
    assert urllib.parse.parse_qs(urllib.parse.urlparse(browser[0]).query)["code"][0] in printed
    assert WALLET[("cocat_rs", server.url)]
    assert "bench" in bench_names(cc.Client(server.url))


def test_the_two_steps_name_the_app_and_its_machine(server):
    client = cc.Client(server.url)

    sign_in = client.start_sign_in()

    assert f"Python on {socket.gethostname()}" in connect_page(server, sign_in.code)
    assert sign_in.poll() == "pending"
    decide(server, sign_in.code)
    sign_in.wait()
    assert "bench" in bench_names(client)


def test_a_denied_sign_in_says_so(server):
    sign_in = cc.Client(server.url).start_sign_in()

    decide(server, sign_in.code, "deny")

    with pytest.raises(cc.NotSignedIn, match="denied"):
        sign_in.wait()
    assert not WALLET


def test_an_expired_sign_in_says_so(server):
    sign_in = cc.Client(server.url).start_sign_in()

    with sqlite3.connect(server.directory / "cocat.db") as database:
        database.execute("UPDATE sign_in_request SET expires_at = '2000-01-01T00:00:00.000Z'")

    with pytest.raises(cc.NotSignedIn, match="expired"):
        sign_in.wait()


def test_a_cancelled_sign_in_stops_waiting(server):
    sign_in = cc.Client(server.url).start_sign_in()

    sign_in.cancel()

    with pytest.raises(cc.NotSignedIn, match="cancelled"):
        sign_in.wait()


def test_without_a_screen_the_browser_is_not_opened(monkeypatch):
    opened = []
    monkeypatch.setattr(client_module.sys, "platform", "linux")
    monkeypatch.delenv("DISPLAY", raising=False)
    monkeypatch.delenv("WAYLAND_DISPLAY", raising=False)
    monkeypatch.setattr(client_module.webbrowser, "open", opened.append)

    client_module._open_browser("https://example.org")

    assert opened == []


def test_logout_revokes_the_token_and_forgets_it(server, signed_in):
    token = WALLET[("cocat_rs", server.url)]

    signed_in.logout()

    assert not WALLET
    with pytest.raises(cc.Unauthorized):
        cc.raw.projects(server.url, token)
    with pytest.raises(cc.NotSignedIn):
        signed_in.projects()


def test_without_a_sign_in_a_call_says_how_to_sign_in(server):
    with pytest.raises(cc.NotSignedIn) as raised:
        cc.Client(server.url).projects()

    assert str(raised.value) == f"Not signed in to {server.url}: run cocat_rs.login()"


def test_a_revoked_token_says_how_to_sign_in_again(server, signed_in):
    cc.raw.sign_out(server.url, WALLET[("cocat_rs", server.url)])

    with pytest.raises(cc.NotSignedIn, match=r"run cocat_rs\.login\(\)"):
        signed_in.projects()
    assert not WALLET


def test_the_server_is_the_argument_else_the_environment_else_the_default(monkeypatch):
    assert cc.Client("https://cocat.example.org/").server == "https://cocat.example.org"
    assert cc.Client().server == cc.DEFAULT_SERVER
    monkeypatch.setenv("COCAT_SERVER", "https://env.example.org")
    assert cc.Client().server == "https://env.example.org"
    assert cc.Client("https://arg.example.org").server == "https://arg.example.org"


def test_without_a_keyring_the_token_lives_for_the_session(server, browser, monkeypatch):
    def no_keyring(*arguments):
        raise keyring.errors.NoKeyringError("no backend")

    for name in ("get_password", "set_password", "delete_password"):
        monkeypatch.setattr(client_module.keyring, name, no_keyring)

    with pytest.warns(UserWarning, match="keyring"):
        cc.Client(server.url).login()

    assert "bench" in bench_names(cc.Client(server.url))


def test_a_password_login_keeps_nothing_in_the_keyring(server):
    client = cc.Client(server.url)

    client.login(server.email, PASSWORD)

    assert "bench" in bench_names(client)
    assert not WALLET


def test_open_by_name_returns_a_synced_project_in_the_users_cache(server, signed_in, monkeypatch, tmp_path):
    monkeypatch.setattr(client_module.platformdirs, "user_cache_dir", lambda name: str(tmp_path / name))

    project = signed_in.open("bench")
    try:
        assert project.status == "synced"
        port = urllib.parse.urlparse(server.url).port
        folder = tmp_path / "cocat_rs" / f"127.0.0.1_{port}" / signed_in.account_id()
        assert any(folder.glob("*.y")), list(tmp_path.rglob("*"))
    finally:
        project.close()


def test_a_project_commits_and_lists_its_history_through_its_client(server, signed_in, cache):
    project = signed_in.open(server.project_id, cache=cache)
    try:
        project.create_event(start="2020-01-01T00:00:00Z", stop="2020-01-01T00:01:00Z")

        version = project.commit(name="paper v1")

        assert project.history()[0]["id"] == version
        assert project.history()[0]["name"] == "paper v1"
    finally:
        project.close()


def test_open_refuses_a_project_the_account_is_not_in(server, signed_in, cache):
    with pytest.raises(cc.Forbidden, match="no-such-project"):
        signed_in.open("no-such-project", cache=cache)


def test_the_module_shortcuts_use_the_default_client(server, browser):
    cc.login(server.url)

    assert "bench" in [project.name for project in cc.projects()]


def test_raw_functions_take_the_server_and_session(server):
    cookie = cc.raw.login(server.url, server.email, PASSWORD)

    assert "bench" in [project.name for project in cc.raw.projects(server.url, cookie)]
    with pytest.raises(TypeError):
        cc.projects(server.url, cookie)


def test_the_signed_in_account_names_its_email(server, signed_in):
    account = signed_in.account()

    assert account["email"] == server.email
    assert account["id"] == signed_in.account_id()
    assert account["is_curator"] is False
    assert cc.raw.account(server.url, server.cookie)["email"] == server.email
