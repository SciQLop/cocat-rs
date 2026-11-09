# Easy login and a simpler Python API

Asked by the user on 2026-11-09: "simplify as much as you can, this is for scientists and
bachelor or master students", a login like Claude Code's (a link the browser confirms),
the public instance by default, and a minimum password strength.

## What a student types

```python
import cocat_rs as cc

cc.login()                         # prints a link (and opens the browser when it can)
project = cc.open("my-project")    # connected and synced
project.history()
cc.projects()
cc.public_catalogues()
```

- No url, no cookie, no password in the script or the notebook.
- `login()` remembers the account in the system keyring, per server. The next session,
  `cc.open(...)` just works. `cc.logout()` forgets it and revokes it on the server.
- Default server: cocat-dev (`https://sciqlop.lpp.polytechnique.fr/cocat-dev`) for now; the
  public instance `/cocat` still runs the Python server. The default moves to `/cocat` in the
  release after production switches (user's choice, 2026-11-09). Another one: `cc.login("https://...")` or the
  `COCAT_SERVER` environment variable.
- `cc.login(email, password)` still works for scripts on machines without a browser.

## Browser sign-in (the "link" login)

1. The client asks the server for a sign-in request. The server answers with a link and a
   short code, valid 10 minutes.
2. The client prints the link and opens it when a browser is at hand.
3. The browser page asks the person to sign in the normal way (password or ORCID), then shows
   "Python on <machine> wants to use your account", with the code, Allow and Deny.
4. The client polls every 2 s. On Allow it receives a token.

Why polling and not a local listener (what Claude Code does): a listener needs the browser and
the client on the same machine. Students on JupyterHub or over SSH don't have that. Polling
works everywhere, and the client still opens the browser itself when it can.

## Tokens

- A token is a long-lived session row with a name ("Python on lpp-jupyter", "SciQLop on
  laptop"), a creation date and a last-used date.
- It stays valid until revoked, or after 6 months unused.
- The account page lists them under "Signed-in apps", each with Revoke.
- A password change keeps them (the person sees and revokes them on purpose).
- Clients send it as `Authorization: Bearer <token>`; the cookie keeps working for browsers.

## Python API

- A `Client` object holds the server and the token. The module functions (`cc.open`,
  `cc.projects`, ...) use a default client built by `login()`. Two servers at once: two
  `Client`s.
- Project-level calls move onto `Project`: `project.history()`, `project.commit(...)`,
  `project.copy_version(...)`, `project.submit(...)`.
- The functions taking `url, cookie` stay as they are, so the SciQLop plugin keeps working.
  The README moves them to an "Advanced" section.

## SciQLop

The plugin gets a "Sign in" button that runs the same browser sign-in and keeps the token in
the keyring. No password field. The plugin session is told; the change is theirs.

## Passwords

At least 8 characters (the user: 10 would make users mad), and not guessable: zxcvbn (established crate) score 3 or more,
which rejects common passwords and simple patterns. No composition rules (NIST 800-63B).
The sign-up and change-password pages show zxcvbn's hint when refusing. Existing passwords keep
working; the rule applies when one is set.

## Left out

- A local-listener login.
- Scopes on tokens (read-only tokens): add when someone needs a token for a shared machine.
