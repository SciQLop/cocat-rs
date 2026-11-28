// A chart: Vega-Lite, drawn by Vega in a sandboxed frame made like the SQL engine's
// (engine.html: opaque origin, a CSP that loads only blob: scripts and connects nowhere), from
// the rows the page hands over and nothing else (docs/design/browser-sql-analysis.md,
// section 4, rule 5). The page refuses a spec naming a URL or data of its own before this frame
// starts (bridge.js); Vega's loader refuses every URL anyway;
// expressions run in Vega's interpreter, as the CSP allows no eval. The answer is SVG text,
// which the page shows as an image: it can run nothing and load nothing.
"use strict";
(() => {
  const reply = (message) => parent.postMessage(message, "*");
  const blobUrl = (bytes, type) => URL.createObjectURL(new Blob([bytes], { type }));
  const refuse = () => Promise.reject(new Error("a chart reaches no URL"));

  const loadScript = (bytes) =>
    new Promise((loaded, failed) => {
      const script = document.createElement("script");
      script.src = blobUrl(bytes, "text/javascript");
      script.onload = loaded;
      script.onerror = () => failed(new Error("Vega did not load"));
      document.head.append(script);
    });

  // vega-interpreter.js imports 'vega-util' by name; the frame has no module resolution, so
  // that import points at a module made here from Vega's UMD build, which holds vega-util.
  async function loadVega({ vega, vegaLite, interpreter }) {
    await loadScript(vega);
    await loadScript(vegaLite);
    const names = Object.keys(self.vega).filter((name) => /^[A-Za-z_$][\w$]*$/.test(name) && name !== "default");
    const shim = names.map((name) => `export const ${name} = globalThis.vega[${JSON.stringify(name)}];`).join("\n");
    const source = new TextDecoder().decode(interpreter).replace("from 'vega-util'", `from "${blobUrl(shim, "text/javascript")}"`);
    return (await import(blobUrl(source, "text/javascript"))).expressionInterpreter;
  }

  // Vega reports a failing expression or transform to its logger, and draws on.
  function collector(warnings, errors) {
    const logger = {
      level: () => logger,
      error: (...parts) => errors.push(parts.map(String).join(" ")),
      warn: (...parts) => warnings.push(parts.map(String).join(" ")),
      info: () => logger,
      debug: () => logger,
    };
    return logger;
  }

  async function draw({ libraries, spec, columns, rows }) {
    const interpreter = await loadVega(libraries);
    const values = rows.map((row) => Object.fromEntries(columns.map((name, at) => [name, row[at]])));
    const warnings = [];
    const errors = [];
    const logger = collector(warnings, errors);
    const compiled = vegaLite.compile({ ...spec, data: { values } }, { logger }).spec;
    const loader = vega.loader();
    Object.assign(loader, { load: refuse, sanitize: refuse, http: refuse, file: refuse });
    const view = new vega.View(vega.parse(compiled, null, { ast: true }), { expr: interpreter, loader, logger, renderer: "none" });
    try {
      await view.runAsync();
      const svg = await view.toSVG();
      if (errors.length) throw new Error(errors.join("; "));
      return { svg, warnings };
    } finally {
      view.finalize();
    }
  }

  addEventListener("message", async (event) => {
    if (event.source !== parent || !event.data || event.data.type !== "draw") return;
    try {
      reply({ type: "answer", id: event.data.id, answer: await draw(event.data) });
    } catch (error) {
      reply({ type: "answer", id: event.data.id, error: String((error && error.message) || error) });
    }
  });
  reply({ type: "loaded" });
})();
