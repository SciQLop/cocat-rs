#!/usr/bin/env bash
# Gate: the SESSION client (the path SciQLop uses) against this server.
#
# The client tests live in cocat-python-tests; `COCAT_SERVER_CMD` makes their `server`
# fixture and their user helpers drive a real binary instead of the ASGI app.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$here/../.." && pwd)
ui=/home/jeandet/Documents/prog/cocat-python-tests
server=${SERVER:-rust}

case "$server" in
    rust) cli=$repo/target/release/cocat-server ;;
    python) cli=$ui/.venv/bin/cocat ;;
    *) echo "SERVER must be rust or python, not $server" >&2; exit 2 ;;
esac
if [[ ! -x $cli ]]; then
    echo "$cli is not executable" >&2
    exit 2
fi

cd "$ui"
COCAT_SERVER_CMD=$cli "$ui/.venv/bin/pytest" tests/test_api.py -q
