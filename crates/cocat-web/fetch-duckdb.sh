#!/usr/bin/env bash
# Puts the SQL engine's third-party files in sql-engine/vendor/, for trunk to copy into the
# bundle: DuckDB-wasm (single-threaded build, DuckDB 2.0.1), its Parquet extension, Apache
# Arrow, which its API imports, and Vega, Vega-Lite and Vega's expression interpreter for
# charts. No DuckDB-wasm build has Parquet built in: it downloads the
# extension at run time, which the sandbox refuses. So we serve the extension ourselves and
# the page hands it to the engine (sql-engine/engine.js).
# Pinned twice: each npm tarball by its integrity hash, each file by its sha256, and
# sql-engine/bridge.js pins the same sha256s, so change both together. The page never loads
# them from a CDN. Run it before `trunk build`; it does nothing when the files are right.
set -euo pipefail
cd "$(dirname "$0")"
VENDOR=sql-engine/vendor

DUCKDB_URL=https://registry.npmjs.org/@duckdb/duckdb-wasm/-/duckdb-wasm-1.33.1-dev124.0.tgz
DUCKDB_SHA512=emdnMQFX+VmuAGA2qb1DbWv6PD4W3v6kzu64UuFIlEcY6z7i0pJwy1AQAfBflnxctagB0TAZsUz/nnjT5dFmTA==
ARROW_URL=https://registry.npmjs.org/apache-arrow/-/apache-arrow-17.0.0.tgz
ARROW_SHA512=X0p7auzdnGuhYMVKYINdQssS4EcKec9TCXyez/qtJt32DrIMGbzqiaMiQ0X6fQlQpw8Fl0Qygcv4dfRAr5Gu9Q==
VEGA_URL=https://registry.npmjs.org/vega/-/vega-6.4.0.tgz
VEGA_SHA512=rHXCq2LpEWCLMjXBEj7hocY9G0/i5TGYRayGGSj06VTF4+l2IG2Z7oeZKCjWbUh/7hTE/mVhbaOPB72wCElh6g==
VEGA_LITE_URL=https://registry.npmjs.org/vega-lite/-/vega-lite-6.5.0.tgz
VEGA_LITE_SHA512=fMXF6s0MrIg0UTWnLIfeFtgSLzVhyzSKobzGrh5rhS+mL6FOYFG3m7ouJ4U23gj65EK+gl9FIIT5w7wLNaAzkA==
INTERPRETER_URL=https://registry.npmjs.org/vega-interpreter/-/vega-interpreter-2.3.2.tgz
INTERPRETER_SHA512=JDAoi3taFcCDLujZG84TNNUXdkAZ5WsSHssx8lWVYaxb9Slsjk7v7PtRIYXpSlUwLaKGRBqoJ9KDs36Z0eMEIw==
# The extension for this build: DuckDB v2.0.1, platform wasm_eh. Signed by DuckDB, which the
# engine checks too.
PARQUET_URL=https://extensions.duckdb.org/v2.0.1/wasm_eh/parquet.duckdb_extension.wasm

SUMS="c9350f8884d0941713f6e747d485dcec851a804d06be3dcce8c28fda632e3371  duckdb-browser.mjs
e7d044df869a7f30f0511eab223396262d0234dc72d3944e4787ae6a73130c40  duckdb-browser-base.worker.js
2acaf8b433bf3adf623c88df96844c8577b1ed3c8556809e766670cba978d144  duckdb-base.wasm
27acb81b1b1b5cf96785deca75da60d05e2267b84b59e8e7e26ea131c8ed587f  Arrow.esnext.min.js
fa88a43aa7366f3ac3b6d0ea6b3a0292c8da45b06070aabcd56da1233078ebcf  parquet.duckdb_extension.wasm
8f6a3587cf8d4f42c7e08120e3eb05d067e746d554e39d2dcf52acc0bd5ba28f  vega.min.js
83e215e84e308ee8688fa6503ba0f1cf8a3c05c0d61df2841a0b92bf86809e00  vega-lite.min.js
c0bdb735a63f9276f136e8d90d7fdf5d8e9df8031eb069048fc9975c876fd7f9  vega-interpreter.js"

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
fetch "$VEGA_URL" "$VEGA_SHA512" vega
fetch "$VEGA_LITE_URL" "$VEGA_LITE_SHA512" vega-lite
fetch "$INTERPRETER_URL" "$INTERPRETER_SHA512" vega-interpreter
rm -rf "$VENDOR"
mkdir -p "$VENDOR"
cp "$work"/duckdb/package/dist/{duckdb-browser.mjs,duckdb-browser-base.worker.js,duckdb-base.wasm} "$VENDOR/"
# duckdb-wasm ships no license file; its package.json says MIT.
cp "$work"/arrow/package/Arrow.esnext.min.js "$VENDOR/"
cp "$work"/arrow/package/LICENSE.txt "$VENDOR/LICENSE-apache-arrow"
for name in vega vega-lite vega-interpreter; do
    cp "$work/$name/package/LICENSE" "$VENDOR/LICENSE-$name"
done
cp "$work"/vega/package/build/vega.min.js "$work"/vega-lite/package/build/vega-lite.min.js \
    "$work"/vega-interpreter/package/build/vega-interpreter.js "$VENDOR/"
curl --fail --silent --show-error --location -o "$VENDOR/parquet.duckdb_extension.wasm" "$PARQUET_URL"
(cd "$VENDOR" && echo "$SUMS" | sha256sum --check --quiet)
echo "SQL engine files in $VENDOR"
