#!/usr/bin/env bash
# Puts the SQL engine's third-party files in sql-engine/vendor/, for trunk to copy into the
# bundle: DuckDB-wasm (single-threaded "base" build, DuckDB 2.0.1, Parquet built in) and Apache
# Arrow, which its API imports. The older 1.33.1-dev57.0 build fetches Parquet from
# extensions.duckdb.org at run time, which the sandbox (rightly) refuses.
# Both are pinned twice: the npm tarball by its integrity hash, each file by its sha256.
# The page never loads them from a CDN. Run it before `trunk build`; it does nothing when the
# files are already there and right.
set -euo pipefail
cd "$(dirname "$0")"
VENDOR=sql-engine/vendor

DUCKDB_URL=https://registry.npmjs.org/@duckdb/duckdb-wasm/-/duckdb-wasm-1.33.1-dev124.0.tgz
DUCKDB_SHA512=emdnMQFX+VmuAGA2qb1DbWv6PD4W3v6kzu64UuFIlEcY6z7i0pJwy1AQAfBflnxctagB0TAZsUz/nnjT5dFmTA==
ARROW_URL=https://registry.npmjs.org/apache-arrow/-/apache-arrow-17.0.0.tgz
ARROW_SHA512=X0p7auzdnGuhYMVKYINdQssS4EcKec9TCXyez/qtJt32DrIMGbzqiaMiQ0X6fQlQpw8Fl0Qygcv4dfRAr5Gu9Q==

SUMS="c9350f8884d0941713f6e747d485dcec851a804d06be3dcce8c28fda632e3371  duckdb-browser.mjs
e7d044df869a7f30f0511eab223396262d0234dc72d3944e4787ae6a73130c40  duckdb-browser-base.worker.js
2acaf8b433bf3adf623c88df96844c8577b1ed3c8556809e766670cba978d144  duckdb-base.wasm
27acb81b1b1b5cf96785deca75da60d05e2267b84b59e8e7e26ea131c8ed587f  Arrow.esnext.min.js"

if [ -d "$VENDOR" ] && (cd "$VENDOR" && echo "$SUMS" | sha256sum --check --status 2>/dev/null); then
    exit 0
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

fetch() { # url sha512 name
    curl --fail --silent --show-error --location -o "$work/$3.tgz" "$1"
    actual=$(openssl dgst -sha512 -binary "$work/$3.tgz" | base64 -w0)
    [ "$actual" = "$2" ] || { echo "$3: the tarball's hash is not the pinned one" >&2; exit 1; }
    mkdir "$work/$3"
    tar xzf "$work/$3.tgz" -C "$work/$3"
}

fetch "$DUCKDB_URL" "$DUCKDB_SHA512" duckdb
fetch "$ARROW_URL" "$ARROW_SHA512" arrow
rm -rf "$VENDOR"
mkdir -p "$VENDOR"
cp "$work"/duckdb/package/dist/{duckdb-browser.mjs,duckdb-browser-base.worker.js,duckdb-base.wasm} "$VENDOR/"
# duckdb-wasm ships no license file; its package.json says MIT.
cp "$work"/arrow/package/Arrow.esnext.min.js "$VENDOR/"
cp "$work"/arrow/package/LICENSE.txt "$VENDOR/LICENSE-apache-arrow"
(cd "$VENDOR" && echo "$SUMS" | sha256sum --check --quiet)
echo "SQL engine files in $VENDOR"
