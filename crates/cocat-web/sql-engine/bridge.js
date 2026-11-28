// window.cocatSql: SQL on the public catalogues' snapshot, run in the sandboxed engine
// (engine.html + engine.js). This side fetches every file with the visitor's session and hands
// the bytes over; the engine never fetches anything. One query runs at a time. A query that
// runs too long is cancelled, and when the engine does not answer the cancel, or dies, the
// frame is thrown away and a fresh one starts with the next query.
// The engine's WebAssembly memory grows with a big query and never shrinks: only dropping the
// engine gives it back to the visitor's machine. So it is dropped after a query that left it
// past `recycleMB`, and after `idleMs` without a query; the next query starts a new one.
"use strict";
(() => {
  const HERE = new URL(".", document.currentScript.src);
  // DuckDB's files, as fetch-duckdb.sh pins them: the page refuses any other bytes, so a
  // server that serves a swapped file starts no engine.
  const ASSETS = {
    arrow: ["vendor/Arrow.esnext.min.js", "27acb81b1b1b5cf96785deca75da60d05e2267b84b59e8e7e26ea131c8ed587f"],
    duckdb: ["vendor/duckdb-browser.mjs", "c9350f8884d0941713f6e747d485dcec851a804d06be3dcce8c28fda632e3371"],
    worker: ["vendor/duckdb-browser-base.worker.js", "e7d044df869a7f30f0511eab223396262d0234dc72d3944e4787ae6a73130c40"],
    wasm: ["vendor/duckdb-base.wasm", "2acaf8b433bf3adf623c88df96844c8577b1ed3c8556809e766670cba978d144"],
  };
  const EXTENSIONS = {
    parquet: ["vendor/parquet.duckdb_extension.wasm", "fa88a43aa7366f3ac3b6d0ea6b3a0292c8da45b06070aabcd56da1233078ebcf"],
  };
  const GRACE_MS = 5000;
  const START_MS = 60000;
  const ATTRIBUTE_FILE = /attributes\/([A-Za-z0-9._-]+)\.parquet/;
  // What a broken WebAssembly heap says: the engine cannot go on after it. DuckDB's own
  // "Out of Memory Error" is not one of them: the query stops, the engine stays.
  const FATAL = /Aborted|out of bounds|unreachable|RuntimeError|memory access/i;
  const OUT_OF_MEMORY = /^Out of Memory Error/;

  let engine = null;
  let idle = null;
  let queue = Promise.resolve();
  let nextId = 0;

  const fetchBytes = async (url) => {
    const response = await fetch(url, { credentials: "same-origin" });
    if (!response.ok) throw new Error(`${url}: ${response.status}`);
    return response.arrayBuffer();
  };

  const hex = (buffer) => Array.from(new Uint8Array(buffer), (byte) => byte.toString(16).padStart(2, "0")).join("");

  async function fetchPinned([path, sha256]) {
    const bytes = await fetchBytes(new URL(path, HERE));
    if (hex(await crypto.subtle.digest("SHA-256", bytes)) !== sha256) {
      throw new Error(`${path} is not the pinned file: its hash differs`);
    }
    return bytes;
  }

  const fetchAll = async (pinned) =>
    Object.fromEntries(await Promise.all(Object.entries(pinned).map(async ([key, file]) => [key, await fetchPinned(file)])));

  // The frame, and a call that sends one message and waits for its answer.
  function openFrame() {
    const frame = document.createElement("iframe");
    frame.setAttribute("sandbox", "allow-scripts");
    frame.hidden = true;
    frame.title = "SQL engine";
    frame.src = new URL("engine.html", HERE).href;
    const waiting = new Map();
    const signals = new Map();
    const listener = (event) => {
      if (event.source !== frame.contentWindow || !event.data) return;
      const { type, id } = event.data;
      if (type === "answer" && waiting.has(id)) {
        waiting.get(id)(event.data);
        waiting.delete(id);
      } else if (signals.has(type)) {
        signals.get(type)(event.data);
      }
    };
    addEventListener("message", listener);
    const signal = (type) => new Promise((done) => signals.set(type, done));
    const ready = signal("ready");
    const loaded = signal("loaded");
    const dead = signal("dead");
    document.body.append(frame);
    const call = (message, transfer = []) =>
      new Promise((done) => {
        const id = nextId++;
        waiting.set(id, done);
        frame.contentWindow.postMessage({ ...message, id }, "*", transfer);
      });
    const close = () => {
      removeEventListener("message", listener);
      frame.remove();
    };
    return { frame, ready, loaded, dead, call, close };
  }

  async function startEngine(snapshotUrl, memoryLimit) {
    const manifest = await (await fetch(snapshotUrl, { credentials: "same-origin" })).json();
    if (!manifest.files || !manifest.files["events.parquet"]) {
      throw new Error("the server is writing a new snapshot of the catalogues: try again in a minute");
    }
    const fileUrl = (logical) => `${snapshotUrl}/${manifest.files[logical].name}`;
    const assets = await fetchAll(ASSETS);
    const extensions = await fetchAll(EXTENSIONS);
    const engineCode = await fetchBytes(new URL("engine.js", HERE));
    const files = {};
    for (const logical of ["events.parquet", "catalogues.parquet"]) files[logical] = await fetchBytes(fileUrl(logical));
    const survey = new TextDecoder().decode(await fetchBytes(fileUrl("survey.sql")));
    const frame = openFrame();
    await frame.ready;
    frame.frame.contentWindow.postMessage({ type: "engine", code: new TextDecoder().decode(engineCode) }, "*");
    await frame.loaded;
    const transfer = [...Object.values(assets), ...Object.values(extensions), ...Object.values(files)];
    const started = await frame.call({ type: "init", assets, extensions, files, survey, memoryLimit }, transfer);
    if (started.error) {
      frame.close();
      throw new Error(`The SQL engine did not start: ${started.error}`);
    }
    return { ...frame, manifest, fileUrl, registered: new Set() };
  }

  // A start that never ends would hold every query after it.
  const withLimit = (promise, ms) =>
    Promise.race([promise, new Promise((_, fail) => setTimeout(() => fail(new Error(`no answer in ${ms / 1000} s`)), ms))]);

  const stop = () => {
    clearTimeout(idle);
    if (engine) engine.then((running) => running.close(), () => {});
    engine = null;
  };

  function release(answer, { recycleMB, idleMs }) {
    if (recycleMB && answer.heapBytes > recycleMB * 2 ** 20) stop();
    else if (engine && idleMs) idle = setTimeout(stop, idleMs);
  }

  // Runs `sql` once; a missing attribute file is fetched, handed over and the query run again.
  async function runOnce(sql, options) {
    clearTimeout(idle);
    if (!engine) engine = withLimit(startEngine(options.snapshot, options.memoryLimit), START_MS);
    let running;
    try {
      running = await engine;
    } catch (error) {
      engine = null;
      return { error: String(error.message || error), kind: "start" };
    }
    for (let attempt = 0; attempt < 8; attempt++) {
      const answer = await withTimeout(running, sql, options);
      const missing = answer.error && ATTRIBUTE_FILE.exec(answer.error);
      const logical = missing && `attributes/${missing[1]}.parquet`;
      if (!logical || running.registered.has(logical)) return answer;
      if (!running.manifest.files[logical]) {
        return { error: `Catalogue ${missing[1]} has no attributes: it has no attribute file.`, kind: "sql" };
      }
      const bytes = await fetchBytes(running.fileUrl(logical));
      await running.call({ type: "register", name: logical, bytes }, [bytes]);
      running.registered.add(logical);
    }
    return { error: "The query names too many attribute files at once.", kind: "sql" };
  }

  async function withTimeout(running, sql, { maxRows, timeoutMs }) {
    let timers = [];
    const outcome = new Promise((done) => {
      timers.push(setTimeout(() => {
        running.frame.contentWindow.postMessage({ type: "cancel" }, "*");
        timers.push(setTimeout(() => done({ kind: "timeout", hung: true }), GRACE_MS));
      }, timeoutMs));
      running.dead.then(() => done({ kind: "dead" }));
      running.call({ type: "query", sql, maxRows }).then((answer) => done({ answer }));
    });
    const { answer, kind, hung } = await outcome;
    timers.forEach(clearTimeout);
    if (kind === "dead") {
      stop();
      return { error: "The SQL engine stopped, most likely out of memory.", kind: "memory" };
    }
    if (hung) {
      stop();
      return { error: `The query ran longer than ${timeoutMs / 1000} s and was stopped.`, kind: "timeout" };
    }
    if (answer.error) {
      if (FATAL.test(answer.error)) {
        stop();
        return { error: answer.error, kind: "memory" };
      }
      if (OUT_OF_MEMORY.test(answer.error)) return { error: answer.error, kind: "memory" };
      const cancelled = /interrupt|cancel/i.test(answer.error);
      return { error: answer.error, kind: cancelled ? "timeout" : "sql" };
    }
    return { ...answer.answer, snapshot: running.manifest.version, created: running.manifest.created };
  }

  window.cocatSql = {
    // options: snapshot (the manifest's URL), maxRows, timeoutMs, memoryLimit ('1GB'),
    // recycleMB and idleMs (both optional).
    // Answers {columns, types, rows, truncated, elapsedMs, heapBytes, snapshot} or
    // {error, kind}, where kind is 'sql', 'timeout', 'memory' or 'start'.
    query(sql, options) {
      const answer = queue.then(async () => {
        const result = await runOnce(sql, options);
        release(result, options);
        return result;
      });
      queue = answer.catch(() => {});
      return answer;
    },
    stop,
  };
})();
