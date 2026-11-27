// The SQL engine: DuckDB-wasm in a sandboxed frame with an opaque origin, no network and no
// way to the page but messages (docs/design/browser-sql-analysis.md, section 4). The page
// sends every byte it needs: DuckDB, Arrow, the snapshot's files. The frame's CSP header
// allows `blob:` scripts and nothing else to load, and DuckDB's own settings are locked so a
// query cannot reach a file or a URL even if the CSP let it.
"use strict";
(() => {
  let duckdb = null;
  let db = null;
  let connection = null;

  const reply = (message) => parent.postMessage(message, "*");
  const blobUrl = (bytes, type) => URL.createObjectURL(new Blob([bytes], { type }));

  // Set once, after the files are registered and survey.sql has run; a query cannot change
  // them. Order matters: the allowed paths cannot change once external access is off.
  const lock = (memoryLimit) => `
    SET threads = 1;
    SET memory_limit = '${memoryLimit}';
    SET autoload_known_extensions = false;
    SET autoinstall_known_extensions = false;
    SET allow_community_extensions = false;
    SET allow_extension_repositories = 'forbidden';
    SET allowed_directories = ['attributes/'];
    SET allowed_paths = ['events.arrow', 'catalogues.arrow'];
    SET enable_external_access = false;
    SET lock_configuration = true;`;

  // duckdb-browser.mjs imports 'apache-arrow' by name; the frame has no module resolution, so
  // that import points at a module made here from Arrow's UMD build.
  async function loadApi(assets) {
    await new Promise((loaded, failed) => {
      const script = document.createElement("script");
      script.src = blobUrl(assets.arrow, "text/javascript");
      script.onload = loaded;
      script.onerror = () => failed(new Error("Arrow did not load"));
      document.head.append(script);
    });
    const names = Object.keys(self.Arrow).filter((name) => /^[A-Za-z_$][\w$]*$/.test(name) && name !== "default");
    const shim = names.map((name) => `export const ${name} = globalThis.Arrow[${JSON.stringify(name)}];`).join("\n");
    const arrow = blobUrl(`${shim}\nexport default globalThis.Arrow;`, "text/javascript");
    const api = new TextDecoder().decode(assets.duckdb).replaceAll('from"apache-arrow"', `from"${arrow}"`);
    return import(blobUrl(api, "text/javascript"));
  }

  // Each step names itself when it fails, so a broken start says where.
  const step = async (name, run) => {
    try {
      return await run();
    } catch (error) {
      throw new Error(`${name}: ${(error && error.message) || error}`);
    }
  };

  async function start({ assets, files, survey, memoryLimit }) {
    if (!/^\d+(MB|GB)$/.test(memoryLimit)) throw new Error("a memory limit like '1GB'");
    duckdb = await step("loading DuckDB", () => loadApi(assets));
    // An opaque origin has no private file system, and asking for it fails DuckDB's start
    // (duckdb-wasm #2302); without the function, DuckDB does not ask.
    const noFileSystem = "if (self.StorageManager) Object.defineProperty(StorageManager.prototype, 'getDirectory', { value: undefined });\n";
    const worker = new Worker(URL.createObjectURL(new Blob([noFileSystem, assets.worker], { type: "text/javascript" })));
    worker.addEventListener("error", () => reply({ type: "dead" }));
    db = new duckdb.AsyncDuckDB(new duckdb.VoidLogger(), worker);
    await step("instantiating", () => db.instantiate(blobUrl(assets.wasm, "application/wasm")));
    await step("opening", () => db.open({
      path: ":memory:",
      query: { castBigIntToDouble: true, castDecimalToDouble: true, castTimestampToDate: true },
    }));
    for (const [name, bytes] of Object.entries(files)) {
      await step(`registering ${name}`, () => db.registerFileBuffer(name, new Uint8Array(bytes)));
    }
    connection = await step("connecting", () => db.connect());
    // The tables survey.sql expects, from the Arrow files: this build of DuckDB reads Arrow
    // IPC on its own, while Parquet support would come from the network.
    await step("naming the files", () => connection.query(`
      CREATE VIEW catalogues AS FROM read_arrow('catalogues.arrow');
      CREATE VIEW events AS FROM read_arrow('events.arrow');
      CREATE MACRO attribute_rows(id) AS TABLE FROM read_arrow('attributes/' || id || '.arrow');`));
    await step("running survey.sql", () => connection.query(survey));
    await step("locking the settings", () => connection.query(lock(memoryLimit)));
  }

  // A cell as plain JSON: times as ISO text, nested values as arrays and objects.
  function plain(value) {
    if (value === null || value === undefined) return null;
    if (value instanceof Date) return Number.isNaN(value.getTime()) ? null : value.toISOString();
    if (typeof value === "bigint") return Number.isSafeInteger(Number(value)) ? Number(value) : value.toString();
    if (typeof value === "number") return Number.isFinite(value) ? value : String(value);
    if (value instanceof Uint8Array) return `<${value.length} bytes>`;
    if (typeof value === "object" && typeof value.toArray === "function") return Array.from(value.toArray(), plain);
    if (typeof value === "object" && typeof value.toJSON === "function") {
      return Object.fromEntries(Object.entries(value.toJSON()).map(([key, item]) => [key, plain(item)]));
    }
    return value;
  }

  async function query({ id, sql, maxRows }) {
    const started = performance.now();
    const reader = await connection.send(sql, true);
    // A stream knows its schema once opened.
    if (!reader.schema) await reader.open();
    const fields = reader.schema ? reader.schema.fields : [];
    // One row past the cap tells whether there are more.
    const rows = [];
    for await (const batch of reader) {
      const take = Math.min(batch.numRows, maxRows + 1 - rows.length);
      for (let row = 0; row < take; row++) {
        rows.push(fields.map((_, column) => plain(batch.getChildAt(column).get(row))));
      }
      if (rows.length > maxRows) break;
    }
    const truncated = rows.length > maxRows;
    if (truncated) {
      rows.pop();
      await connection.cancelSent().catch(() => {});
    }
    return {
      columns: fields.map((field) => field.name),
      types: fields.map((field) => String(field.type)),
      rows,
      truncated,
      elapsedMs: Math.round(performance.now() - started),
    };
  }

  const handlers = {
    init: start,
    register: ({ name, bytes }) => db.registerFileBuffer(name, new Uint8Array(bytes)),
    query,
    cancel: () => connection.cancelSent(),
  };

  addEventListener("message", async (event) => {
    if (event.source !== parent || !event.data || !(event.data.type in handlers)) return;
    const { type, id } = event.data;
    try {
      const answer = await handlers[type](event.data);
      if (id !== undefined) reply({ type: "answer", id, answer: answer ?? null });
    } catch (error) {
      if (id !== undefined) reply({ type: "answer", id, error: String((error && error.message) || error) });
    }
  });
  reply({ type: "loaded" });
})();
