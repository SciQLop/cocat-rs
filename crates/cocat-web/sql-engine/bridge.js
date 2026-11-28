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
  const CHARTS = {
    vega: ["vendor/vega.min.js", "8f6a3587cf8d4f42c7e08120e3eb05d067e746d554e39d2dcf52acc0bd5ba28f"],
    vegaLite: ["vendor/vega-lite.min.js", "83e215e84e308ee8688fa6503ba0f1cf8a3c05c0d61df2841a0b92bf86809e00"],
    interpreter: ["vendor/vega-interpreter.js", "c0bdb735a63f9276f136e8d90d7fdf5d8e9df8031eb069048fc9975c876fd7f9"],
  };
  const CHART_ROWS = 5000;
  const CHART_MS = 15000;
  const GRACE_MS = 5000;
  const START_MS = 60000;
  const ATTRIBUTE_FILE = /attributes\/([A-Za-z0-9._-]+)\.parquet/;
  // What a broken WebAssembly heap says: the engine cannot go on after it. DuckDB's own
  // "Out of Memory Error" is not one of them: the query stops, the engine stays.
  const FATAL = /Aborted|out of bounds|unreachable|RuntimeError|memory access/i;
  const OUT_OF_MEMORY = /^Out of Memory Error/;

  let engine = null;
  let idle = null;
  let chartLibraries = null;
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
  function openFrame(title) {
    const frame = document.createElement("iframe");
    frame.setAttribute("sandbox", "allow-scripts");
    frame.hidden = true;
    frame.title = title;
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

  // The frame is opened by the caller, so that a start past its time closes it.
  async function startEngine(frame, snapshotUrl, memoryLimit) {
    const manifest = await (await fetch(snapshotUrl, { credentials: "same-origin" })).json();
    if (!manifest.files || !manifest.files["events.parquet"]) {
      throw new Error("the server is writing a new snapshot of the catalogues: try again in a minute");
    }
    const fileUrl = (logical) => `${snapshotUrl}/${manifest.files[logical].name}`;
    const assets = await fetchAll(ASSETS);
    const extensions = await fetchAll(EXTENSIONS);
    const files = {};
    for (const logical of ["events.parquet", "catalogues.parquet"]) files[logical] = await fetchBytes(fileUrl(logical));
    const survey = new TextDecoder().decode(await fetchBytes(fileUrl("survey.sql")));
    await startIn(frame, "engine.js");
    const transfer = [...Object.values(assets), ...Object.values(extensions), ...Object.values(files)];
    const started = await frame.call({ type: "init", assets, extensions, files, survey, memoryLimit }, transfer);
    if (started.error) throw new Error(`The SQL engine did not start: ${started.error}`);
    return { ...frame, manifest, fileUrl, registered: new Set() };
  }

  // A start that never ends would hold every query after it.
  const withLimit = (promise, ms) =>
    Promise.race([promise, new Promise((_, fail) => setTimeout(() => fail(new Error(`no answer in ${ms / 1000} s`)), ms))]);

  function startClosingOnFailure({ snapshot, memoryLimit }) {
    const frame = openFrame("SQL engine");
    return withLimit(startEngine(frame, snapshot, memoryLimit), START_MS).catch((error) => {
      frame.close();
      throw error;
    });
  }

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
    if (!engine) engine = startClosingOnFailure(options);
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

  // Starts `file` in an opened frame (engine.html).
  async function startIn(frame, file) {
    const code = await fetchBytes(new URL(file, HERE));
    await frame.ready;
    frame.frame.contentWindow.postMessage({ type: "engine", code: new TextDecoder().decode(code) }, "*");
    await frame.loaded;
  }

  // Each chart gets a fresh frame (chart.js): it keeps nothing between charts, and it never
  // waits for the engine, which may have just been dropped.
  async function draw(frame, spec, answer) {
    if (!chartLibraries) chartLibraries = fetchAll(CHARTS).catch((error) => { chartLibraries = null; throw error; });
    const libraries = await chartLibraries;
    await startIn(frame, "chart.js");
    const rows = answer.rows.slice(0, CHART_ROWS);
    const drawn = await frame.call({ type: "draw", libraries, spec, columns: answer.columns, rows });
    if (drawn.error) return { error: drawn.error };
    const warnings = [...drawn.answer.warnings];
    if (answer.rows.length > CHART_ROWS) warnings.unshift(`only the first ${CHART_ROWS} of ${answer.rows.length} rows are plotted`);
    if (answer.truncated) warnings.unshift(`the query has more rows than the ${answer.rows.length} fetched: aggregate it`);
    return { svg: drawn.answer.svg, rowsPlotted: rows.length, warnings };
  }

  // What a chart may not hold: its rows come from the query only, and its work stays in
  // proportion to them. Checked before any frame starts.
  // simplify: a deny list of what Vega-Lite 6.5 can generate or multiply. A new generator in a
  // later Vega-Lite gets through it; then only the time limit stops the chart, and its frame
  // is dropped with it. Recheck this list when the pinned Vega-Lite changes.
  const REFUSED_KEYS = {
    url: "a chart reaches no URL",
    href: "a chart reaches no URL",
    usermeta: "a chart takes no embed options",
    data: "a chart takes its data from the query only",
    datasets: "a chart takes its data from the query only",
    sequence: "a chart takes its data from the query only",
    graticule: "a chart takes its data from the query only",
    sphere: "a chart takes its data from the query only",
    flatten: "unnest lists in the SQL query instead",
  };
  const AT_MOST = {
    steps: 200, minsteps: 200, maxsteps: 200, maxbins: 200, tickCount: 100, count: 100, nice: 100,
    width: 4000, height: 4000, continuousWidth: 4000, continuousHeight: 4000, discreteWidth: 4000, discreteHeight: 4000,
  };
  const FOLD_FIELDS = 20;
  const QUANTILE_STEP = 0.01;
  const MAKES_ARRAYS = /\b(sequence|pad)\s*\(/;

  function objectProblem(object, parentKey) {
    for (const [key, value] of Object.entries(object)) {
      if (key in REFUSED_KEYS) return `its spec may not hold "${key}": ${REFUSED_KEYS[key]}`;
      if (typeof value === "number" && key in AT_MOST && value > AT_MOST[key]) return `"${key}" is at most ${AT_MOST[key]}`;
    }
    if (parentKey === "bin" && "step" in object) return 'a bin takes "maxbins", not "step"';
    if (object.keyvals && !Array.isArray(object.keyvals)) return '"keyvals" is a list of values';
    if (Array.isArray(object.fold) && object.fold.length > FOLD_FIELDS) return `"fold" takes at most ${FOLD_FIELDS} fields`;
    if ("quantile" in object && object.step < QUANTILE_STEP) return `a quantile "step" is at least ${QUANTILE_STEP}`;
    return undefined;
  }

  function specProblem(value, parentKey) {
    if (typeof value === "string") return MAKES_ARRAYS.test(value) ? "an expression may not call sequence() or pad()" : undefined;
    if (Array.isArray(value)) return value.map((item) => specProblem(item, parentKey)).find(Boolean);
    if (!value || typeof value !== "object") return undefined;
    return objectProblem(value, parentKey) || Object.entries(value).map(([key, item]) => specProblem(item, key)).find(Boolean);
  }

  // The frame opens before the time limit starts and closes whatever happens, so a chart past
  // its time stops working in the visitor's tab.
  async function plot(spec, answer, { timeoutMs = CHART_MS } = {}) {
    const problem = specProblem(spec);
    if (problem) return { error: problem };
    const frame = openFrame("Chart");
    try {
      return await withLimit(draw(frame, spec, answer), timeoutMs);
    } catch (error) {
      return { error: String((error && error.message) || error) };
    } finally {
      frame.close();
    }
  }

  window.cocatSql = {
    // A Vega-Lite `spec` drawn from `answer` (a query's answer: columns and rows), as
    // {svg, rowsPlotted, warnings} or {error}. options: timeoutMs (optional).
    plot,
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
