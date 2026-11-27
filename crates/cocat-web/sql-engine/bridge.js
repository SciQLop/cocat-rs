// window.cocatSql: SQL on the public catalogues' snapshot, run in the sandboxed engine
// (engine.html + engine.js). This side fetches every file with the visitor's session and hands
// the bytes over; the engine never fetches anything. One query runs at a time. A query that
// runs too long is cancelled, and when the engine does not answer the cancel, or dies, the
// frame is thrown away and a fresh one starts with the next query.
"use strict";
(() => {
  const HERE = new URL(".", document.currentScript.src);
  const ASSETS = {
    engine: "engine.js",
    arrow: "vendor/Arrow.esnext.min.js",
    duckdb: "vendor/duckdb-browser.mjs",
    worker: "vendor/duckdb-browser-base.worker.js",
    wasm: "vendor/duckdb-base.wasm",
  };
  const GRACE_MS = 5000;
  const START_MS = 60000;
  const ATTRIBUTE_FILE = /attributes\/([A-Za-z0-9._-]+)\.arrow/;
  // What a broken WebAssembly heap says: the engine cannot go on after it. DuckDB's own
  // "Out of Memory Error" is not one of them: the query stops, the engine stays.
  const FATAL = /Aborted|out of bounds|unreachable|RuntimeError|memory access/i;
  const OUT_OF_MEMORY = /^Out of Memory Error/;

  let engine = null;
  let queue = Promise.resolve();
  let nextId = 0;

  const fetchBytes = async (url) => {
    const response = await fetch(url, { credentials: "same-origin" });
    if (!response.ok) throw new Error(`${url}: ${response.status}`);
    return response.arrayBuffer();
  };

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
    if (!manifest.files || !manifest.files["events.arrow"]) {
      throw new Error("the server is writing a new snapshot of the catalogues: try again in a minute");
    }
    const fileUrl = (logical) => `${snapshotUrl}/${manifest.files[logical].name}`;
    const assets = Object.fromEntries(
      await Promise.all(Object.entries(ASSETS).map(async ([key, path]) => [key, await fetchBytes(new URL(path, HERE))])),
    );
    const files = {};
    for (const logical of ["events.arrow", "catalogues.arrow"]) files[logical] = await fetchBytes(fileUrl(logical));
    const survey = new TextDecoder().decode(await fetchBytes(fileUrl("survey.sql")));
    const frame = openFrame();
    await frame.ready;
    frame.frame.contentWindow.postMessage({ type: "engine", code: new TextDecoder().decode(assets.engine) }, "*");
    await frame.loaded;
    delete assets.engine;
    const transfer = [...Object.values(assets), ...Object.values(files)];
    const started = await frame.call({ type: "init", assets, files, survey, memoryLimit }, transfer);
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
    if (engine) engine.then((running) => running.close(), () => {});
    engine = null;
  };

  // Runs `sql` once; a missing attribute file is fetched, handed over and the query run again.
  async function runOnce(sql, options) {
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
      const logical = missing && `attributes/${missing[1]}.arrow`;
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
    // options: snapshot (the manifest's URL), maxRows, timeoutMs, memoryLimit ('1GB').
    // Answers {columns, types, rows, truncated, elapsedMs, snapshot} or {error, kind}, where
    // kind is 'sql', 'timeout', 'memory' or 'start'.
    query(sql, options) {
      const answer = queue.then(() => runOnce(sql, options));
      queue = answer.catch(() => {});
      return answer;
    },
    stop,
  };
})();
