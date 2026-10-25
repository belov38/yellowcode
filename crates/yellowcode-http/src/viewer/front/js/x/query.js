import { createEditor, replaceText, goToLine, cm } from "../editor.js";
import { el, fill, clear, append } from "../dom.js";
import { xget, xpost, isErrorOut } from "./net.js";
import { createTable, PAGE } from "./table.js";
import { createParams } from "./query_params.js";
import { activate, activeWindow, openWindow, update } from "../tabs.js";
import { OK_EVENT, openBuilder } from "./qb.js";
import { laterStrip, openBaseMessage, tx, waitingBlock } from "./waiting.js";

const POLL_MS = 120;
const MAX_HISTORY = 200;
const MAX_TEXT = 64 * 1024;
const MAX_PARAMS = 16 * 1024;
const byteLen = (s) => new TextEncoder().encode(s).length;


function lsGet(key) {
  try {
    return window.localStorage.getItem(key);
  } catch (e) {
    return null;
  }
}

function lsSet(key, value) {
  try {
    window.localStorage.setItem(key, value);
  } catch (e) {
  }
}

function hashOfText(s) {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619) >>> 0;
  }
  return h.toString(16);
}

function makeHistory(key) {
  let list = [];
  try {
    const raw = lsGet(key);
    const j = raw ? JSON.parse(raw) : [];
    if (Array.isArray(j)) list = j.filter((e) => e && typeof e.text === "string");
  } catch (e) {
    list = [];
  }
  const save = () => lsSet(key, JSON.stringify(list));
  return {
    list: () => list,
    add(entry) {
      if (byteLen(entry.text) > MAX_TEXT) return;
      if (JSON.stringify(entry.params || {}).length > MAX_PARAMS) entry.params = {};
      const prev = list.find((e) => !e.pin && e.text === entry.text);
      if (prev) list.splice(list.indexOf(prev), 1);
      entry.id = Date.now().toString(36) + Math.random().toString(36).slice(2, 6);
      list.unshift(entry);
      while (list.length > MAX_HISTORY) {
        let k = list.length - 1;
        while (k >= 0 && list[k].pin) k--;
        list.splice(k < 0 ? list.length - 1 : k, 1);
      }
      save();
    },
    remove(id) {
      list = list.filter((e) => e.id !== id);
      save();
    },
    togglePin(id) {
      const e = list.find((x) => x.id === id);
      if (e) e.pin = !e.pin;
      save();
    },
    clear() {
      list = list.filter((e) => e.pin);
      save();
    },
  };
}

function drag(handle, axis, onMove, onEnd) {
  let on = false;
  let start = 0;
  handle.addEventListener("pointerdown", (e) => {
    on = true;
    start = axis === "y" ? e.clientY : e.clientX;
    handle.setPointerCapture(e.pointerId);
    e.preventDefault();
  });
  handle.addEventListener("pointermove", (e) => {
    if (on) onMove((axis === "y" ? e.clientY : e.clientX) - start);
  });
  const stop = () => {
    if (on && onEnd) onEnd();
    on = false;
  };
  handle.addEventListener("pointerup", stop);
  handle.addEventListener("pointercancel", stop);
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const fmtMs = (ms) => (ms < 1000 ? ms + " " + tx("мс", "ms") : (ms / 1000).toFixed(1) + " " + tx("с", "s"));
const lang = () => (document.documentElement.lang === "en" ? "en" : "ru");
const oneLine = (s, n) => {
  const t = s.replace(/\s+/g, " ").trim();
  return t.length > n ? t.slice(0, n) : t;
};

const PHASE = {
  compile: () => tx("Ошибка компиляции", "Compile error"),
  runtime: () => tx("Ошибка выполнения", "Runtime error"),
  params: () => tx("Ошибка параметров", "Parameter error"),
};

const QUERY_PATH = "x/query";
const QUERY_KIND = "x-query";
const consoles = new Map();

window.addEventListener(OK_EVENT, (ev) => {
  const d = ev.detail;
  if (!d || typeof d.text !== "string" || d.handled) return;
  const targeted = d.target !== null && d.target !== undefined;
  let rec = targeted ? consoles.get(d.target) : null;
  if (targeted && !rec) return;
  if (targeted && typeof d.originText === "string" && (!rec.ctl || rec.ctl.getText() !== d.originText)) {
    d.error = tx("Текст исходной консоли изменён. Запрос конструктора не применён.", "The source console text has changed. The builder query was not applied.");
    return;
  }
  if (!rec) {
    const a = activeWindow();
    rec = (a && consoles.get(a.id)) || consoles.values().next().value || null;
  }
  if (!rec) {
    openWindow({ kind: QUERY_KIND, path: QUERY_PATH, title: tx("Консоль запросов", "Query console") });
    rec = consoles.values().next().value || null;
    if (!rec) return;
  }
  d.handled = true;
  activate(rec.win);
  if (rec.ctl) rec.ctl.setText(d.text, !!d.run);
  else rec.queued = { text: d.text, run: !!d.run };
});

export function createQueryTab(win) {
  const host = el("div", { class: "xq-host" }, el("div", { class: "msg" }, tx("Загрузка…", "Loading…")));
  win.pane.appendChild(host);
  update(win, { title: tx("Консоль запросов", "Query console") });
  let ctl = null;
  const rec = { win, ctl: null, queued: null };
  consoles.set(win.id, rec);
  const open = () =>
    xget("info").then(
      (info) => {
        if (win.closed) return;
        if (info.enabled === false) return fill(host, openBaseMessage(info.reason));
        if (info.error) {
          return fill(host, el("div", { class: "xq-notbase" },
            el("div", { class: "msg bad", "data-wait": "open-failed" }, tx("База не открыта: ", "The base is not open: ") + info.error),
            el("button", { class: "btn", type: "button", onclick: () => {
              fill(host, el("div", { class: "msg" }, tx("Загрузка…", "Loading…")));
              xpost("reopen", {}).then(open, (e) => fill(host, el("div", { class: "msg bad" }, e.message)));
            } }, tx("Открыть заново", "Open again"))));
        }
        ctl = buildConsole(win, host, info);
        rec.ctl = ctl;
        if (rec.queued) ctl.setText(rec.queued.text, rec.queued.run);
        rec.queued = null;
      },
      (e) => fill(host, el("div", { class: "msg bad" }, tx("Ошибка: ", "Error: ") + e.message)),
    );
  open();
  return { destroy() { consoles.delete(win.id); if (ctl) ctl.destroy(); } };
}

function buildConsole(win, host, info) {
  const base = "x.q." + hashOfText(info.path || "");
  const hist = makeHistory(base + ".hist");
  const sizeKey = "x.q.size";
  let sizes = {};
  try {
    sizes = JSON.parse(lsGet(sizeKey) || "{}") || {};
  } catch (e) {
    sizes = {};
  }
  const saveSizes = () => lsSet(sizeKey, JSON.stringify(sizes));

  let jobId = null;
  let running = false;
  let dead = false;
  let startedAt = 0;
  let tableCtl = null;
  let explainSeq = 0;
  let stopRequested = false;

  const runBtn = el("button", { class: "btn xq-run", type: "button", title: "Ctrl+Enter", onclick: () => run() }, tx("Выполнить", "Run"));
  const stopBtn = el("button", { class: "btn xq-stop", type: "button", disabled: true, onclick: () => stop() }, tx("Остановить", "Stop"));
  const planBtn = el("button", { class: "btn xq-plan-btn", type: "button", onclick: () => explain() }, tx("План", "Plan"));
  const mode = el("select", { class: "xq-mode", "aria-label": tx("Режим", "Mode") },
    el("option", { value: "batch" }, tx("Пакет", "Batch")),
    el("option", { value: "one" }, tx("Последний результат", "Last result")),
    el("option", { value: "inter" }, tx("С промежуточными данными", "With intermediate data")));
  const histBtn = el("button", { class: "btn xq-hist-btn", type: "button", onclick: () => toggle(histPane) }, tx("История", "History"));
  const qbBtn = el("button", { class: "btn xq-qb-btn", type: "button", title: tx("Открыть текст в конструкторе запросов", "Open the text in the query builder"), onclick: () => openBuilder({ text: getText(), origin: win.id, originText: getText() }) }, tx("Конструктор", "Builder"));
  const stat = el("span", { class: "xq-stat dim", "aria-live": "polite" });
  const bar = el("div", { class: "xq-bar" }, runBtn, stopBtn, planBtn, mode, histBtn, qbBtn, stat);

  const ed = el("div", { class: "xq-ed cm-host" });
  const editor = createEditor({ parent: ed, queryLanguage: true,
    onChange() { clearMark(); saveDraft(); params.schedule(); },
    keys: [{ key: "Mod-Enter", run() { run(); return true; } }],
  });
  const getText = () => editor.state.doc.toString();
  const setText = (text) => replaceText(editor, text);
  const edH = Math.max(80, Math.min(sizes.edH || 170, 800));
  ed.style.height = edH + "px";
  const split1 = el("div", { class: "xq-split xq-split-y", role: "separator", "aria-orientation": "horizontal" });
  function clearMark() { editor.dispatch(cm.setDiagnostics(editor.state, [])); }
  function mark(line, col) {
    if (line < 1 || line > editor.state.doc.lines) return;
    const l = editor.state.doc.line(line);
    const from = Math.min(l.to, l.from + Math.max(0, col - 1));
    editor.dispatch(cm.setDiagnostics(editor.state, [{ from, to: Math.min(l.to, from + 1), severity: "error", message: tx("Ошибка запроса", "Query error") }]));
    goToLine(editor, line, col);
  }
  drag(split1, "y", (d) => {
    ed.style.height = Math.max(80, Math.min(edH + d, 800)) + "px";
  }, () => {
    sizes.edH = ed.offsetHeight;
    saveSizes();
  });

  const params = createParams({ getText: () => getText(), onChange: () => {} });
  const errs = el("div", { class: "xq-errs" });
  const rtabs = el("div", { class: "xq-rtabs", role: "tablist" });
  const rbody = el("div", { class: "xq-rbody" });
  const res = el("div", { class: "xq-res" }, rtabs, rbody);

  function showError(e, where) {
    const box = where || errs;
    if (e.phase === "later") {
      box.appendChild(laterStrip(e.chain));
      return;
    }
    const lab = PHASE[e.phase] ? PHASE[e.phase]() : tx("Ошибка", "Error");
    const lines = (e.chain || []).map((c, i) => el("div", { class: "xq-err-line", style: "padding-left:" + i * 12 + "px" }, String(c)));
    box.appendChild(el("div", { class: "xq-err", role: "alert", "data-phase": e.phase }, el("div", { class: "xq-err-head" }, lab + (e.at ? " {(" + e.at[0] + ", " + e.at[1] + ")}" : "")), lines));
    if (e.at) mark(e.at[0], e.at[1]);
  }
  function plainError(text) {
    errs.appendChild(el("div", { class: "xq-err", role: "alert" }, el("div", { class: "xq-err-line" }, text)));
  }

  const planBody = el("div", { class: "xq-plan-body" }, el("div", { class: "msg" }, tx("Нажмите «План»", "Press “Plan”")), waitingBlock());
  const planPane = el("aside", { class: "xq-plan", hidden: true }, el("div", { class: "xq-pane-head" }, tx("План и IR", "Plan and IR")), planBody);
  const split2 = el("div", { class: "xq-split xq-split-x", role: "separator", "aria-orientation": "vertical", hidden: true });
  const planW = () => Math.max(220, Math.min(sizes.planW || 380, 900));
  planPane.style.width = planW() + "px";
  drag(split2, "x", (d) => {
    planPane.style.width = Math.max(220, Math.min(planW() - d, 900)) + "px";
  }, () => {
    sizes.planW = planPane.offsetWidth;
    saveSizes();
  });

  const histList = el("div", { class: "xq-hist-list" });
  const histPane = el("aside", { class: "xq-hist", hidden: true },
    el("div", { class: "xq-pane-head" }, tx("История", "History"),
      el("button", { class: "btn xq-hist-clear", type: "button", onclick: () => { hist.clear(); renderHist(); } }, tx("Очистить", "Clear"))),
    histList);
  histPane.style.width = Math.max(200, Math.min(sizes.histW || 280, 600)) + "px";
  const split3 = el("div", { class: "xq-split xq-split-x", role: "separator", "aria-orientation": "vertical", hidden: true });
  drag(split3, "x", (d) => {
    histPane.style.width = Math.max(200, Math.min((sizes.histW || 280) + d, 600)) + "px";
  }, () => {
    sizes.histW = histPane.offsetWidth;
    saveSizes();
  });

  function toggle(pane) {
    pane.hidden = !pane.hidden;
    if (pane === histPane) {
      split3.hidden = pane.hidden;
      if (!pane.hidden) renderHist();
    } else {
      split2.hidden = pane.hidden;
    }
  }

  function renderHist() {
    const items = hist.list().slice().sort((a, b) => (b.pin ? 1 : 0) - (a.pin ? 1 : 0));
    if (!items.length) return fill(histList, el("div", { class: "msg" }, tx("История пуста", "No history yet")));
    fill(histList, items.map((e) => {
      const d = new Date(e.ts || 0);
      const when = isNaN(d.getTime()) ? "" : d.toLocaleString();
      const meta = [when, e.rows != null ? e.rows + " " + tx("стр.", "rows") : null, e.ms != null ? fmtMs(e.ms) : null].filter(Boolean).join(" · ");
      return el("div", { class: "xq-hent" + (e.pin ? " pin" : ""), "data-id": e.id },
        el("div", { class: "xq-hmain", role: "button", tabindex: "0", title: e.text.slice(0, 500), onclick: () => load(e) },
          el("div", { class: "xq-htext" }, oneLine(e.text, 80)),
          el("div", { class: "xq-hmeta dim" }, meta)),
        el("div", { class: "xq-hact" },
          el("button", { class: "xq-ibtn xq-h-again", type: "button", title: tx("повторить", "run again"), onclick: () => { load(e).then(run); } }, "↻"),
          el("button", { class: "xq-ibtn xq-h-pin", type: "button", title: e.pin ? tx("открепить", "unpin") : tx("закрепить", "pin"), onclick: () => { hist.togglePin(e.id); renderHist(); } }, e.pin ? "★" : "☆"),
          el("button", { class: "xq-ibtn xq-h-del", type: "button", title: tx("удалить", "delete"), onclick: () => { hist.remove(e.id); renderHist(); } }, "×")));
    }));
  }

  function load(e) {
    setText(e.text);
    if (e.mode) mode.value = e.mode;
    clearMark();
    params.setState(e.params || {});
    saveDraft();
    return params.refresh();
  }

  function saveDraft() {
    lsSet(base + ".draft", JSON.stringify({ text: byteLen(getText()) <= MAX_TEXT ? getText() : "", mode: mode.value }));
  }
  mode.addEventListener("change", saveDraft);

  function setRunning(on) {
    running = on;
    runBtn.disabled = on;
    stopBtn.disabled = !on;
    planBtn.disabled = on;
  }

  function request() {
    const v = params.values();
    return { text: getText(), params: v.params, mode: mode.value, lang: lang(), errors: v.errors };
  }

  function resetOutput() {
    clear(errs);
    clearMark();
    clear(rtabs);
    clear(rbody);
    if (tableCtl) tableCtl.destroy();
    tableCtl = null;
  }

  async function run() {
    if (running || dead) return;
    if (!getText().trim()) return;
    setRunning(true);
    jobId = null;
    stopRequested = false;
    try {
      await params.flush();
    } catch (e) {
    }
    if (dead) return;
    const body = request();
    resetOutput();
    if (body.errors.length) {
      body.errors.forEach(plainError);
      setRunning(false);
      return;
    }
    delete body.errors;
    const paramState = params.state();
    startedAt = performance.now();
    stat.textContent = tx("Выполняется…", "Running…");
    const tick = setInterval(() => {
      stat.textContent = tx("Выполняется… ", "Running… ") + fmtMs(Math.round(performance.now() - startedAt));
    }, 200);
    let job = null;
    try {
      const { id } = await xpost("run", body);
      jobId = id;
      if (stopRequested) xpost("cancel", {}, { id }).catch(() => {});
      for (;;) {
        job = await xget("job", { id });
        if (job.state !== "running" || dead) break;
        await sleep(POLL_MS);
      }
    } catch (e) {
      plainError(tx("Ошибка: ", "Error: ") + e.message);
    }
    clearInterval(tick);
    setRunning(false);
    if (dead) return;
    if (!job) {
      stat.textContent = "";
      return;
    }
    showJob(job, body, paramState);
  }

  function stop() {
    if (!running) return;
    if (jobId === null) {
      stopRequested = true;
      return;
    }
    xpost("cancel", {}, { id: jobId }).catch(() => {});
  }

  function showJob(job, body, paramState) {
    const rep = job.report;
    const stmts = rep ? rep.statements || [] : [];
    const ms = rep ? rep.elapsed_ms : job.elapsed_ms;
    if (job.state === "cancelled") {
      errs.appendChild(laterStrip([tx("Выполнение остановлено", "Execution stopped")]));
      errs.firstChild.classList.add("xq-stopped");
    }
    if (job.state === "failed" && job.error) showError(job.error);
    if (rep && rep.error) showError(rep.error);
    let results = 0;
    const tabs = stmts.map((s, i) => {
      if (s.type === "result") {
        results++;
        return { stmt: i, s, title: tx("Результат ", "Result ") + results };
      }
      if (s.type === "count") return { stmt: i, s, title: tx("Количество: ", "Count: ") + s.n + " (" + tx("ПОМЕСТИТЬ", "INTO") + ")" };
      return { stmt: i, s, title: tx("Уничтожена", "Dropped") };
    });
    const firstResult = stmts.find((s) => s.type === "result");
    const rows = firstResult ? firstResult.total : null;
    const label = job.state === "cancelled" ? tx("остановлено", "stopped") : null;
    stat.textContent = [rows !== null ? rows + " " + tx("строк", "rows") : null, fmtMs(ms), label].filter(Boolean).join(" · ");
    hist.add({ ts: Date.now(), text: body.text, mode: body.mode, params: paramState, rows, ms });
    if (!histPane.hidden) renderHist();
    if (!tabs.length) return;
    const btns = [];
    const select = (k) => {
      btns.forEach((b, j) => {
        b.classList.toggle("cur", j === k);
        b.setAttribute("aria-selected", j === k ? "true" : "false");
      });
      showStmt(jobId, tabs[k]);
    };
    tabs.forEach((t, k) => {
      const b = el("button", { class: "xq-rtab", type: "button", role: "tab", title: t.s.type === "count" ? t.s.name : null, onclick: () => select(k) }, t.title);
      btns.push(b);
      rtabs.appendChild(b);
    });
    select(0);
  }

  function showStmt(id, t) {
    if (tableCtl) tableCtl.destroy();
    tableCtl = null;
    clear(rbody);
    if (t.s.type === "count") {
      return rbody.appendChild(el("div", { class: "msg", "data-count": t.s.n }, tx("Во временную таблицу «", "Rows put into the temporary table “") + t.s.name + tx("» помещено строк: ", "”: ") + t.s.n));
    }
    if (t.s.type === "dropped") return rbody.appendChild(el("div", { class: "msg" }, tx("Временная таблица уничтожена", "The temporary table was dropped")));
    const s = t.s;
    tableCtl = createTable({
      columns: s.columns, total: s.total, kept: s.kept === undefined ? s.total : s.kept, truncated: s.truncated,
      fetch: (from, limit) => xget("rows", { id, stmt: t.stmt, from, limit }),
      nested: (row, col) => xget("nested", { id, stmt: t.stmt, row, col }),
    });
    rbody.appendChild(tableCtl.root);
  }

  async function explain() {
    if (running || !getText().trim()) return;
    try {
      await params.flush();
    } catch (e) {
    }
    if (dead) return;
    const body = request();
    clear(errs);
    if (planPane.hidden) toggle(planPane);
    if (body.errors.length) {
      body.errors.forEach(plainError);
      return;
    }
    delete body.errors;
    const mine = ++explainSeq;
    fill(planBody, el("div", { class: "msg" }, tx("Загрузка…", "Loading…")));
    try {
      const a = await xpost("explain", body);
      if (mine !== explainSeq || dead) return;
      clear(planBody);
      if (isErrorOut(a)) {
        showError(a, planBody);
      } else {
        append(planBody, [
          el("h4", null, tx("План исполнения (1CD)", "Execution plan (1CD)")),
          el("pre", { class: "xq-pre xq-plan-text" }, a.plan || ""),
          el("h4", null, "IR-S"),
          el("pre", { class: "xq-pre xq-ir-text" }, a.ir || ""),
          (a.notes || []).filter((n) => !/^IR-L/.test(n)).map((n) => el("div", { class: "dim xq-note" }, n)),
        ]);
      }
      planBody.appendChild(waitingBlock());
    } catch (e) {
      if (mine === explainSeq) fill(planBody, el("div", { class: "msg bad" }, tx("Ошибка: ", "Error: ") + e.message));
    }
  }

  const center = el("div", { class: "xq-center" }, ed, split1, params.root, errs, res);
  const body = el("div", { class: "xq-body" }, histPane, split3, center, split2, planPane);
  fill(host, el("div", { class: "xq" }, bar, body));

  try {
    const d = JSON.parse(lsGet(base + ".draft") || "null");
    if (d && typeof d.text === "string") setText(d.text);
    if (d && ["batch", "one", "inter"].includes(d.mode)) mode.value = d.mode;
  } catch (e) {
  }
  if (getText().trim()) params.refresh();

  return {
    getText: () => getText(),
    setText(text, andRun) {
      if (dead) return;
      setText(text);
      clearMark();
      saveDraft();
      params.refresh();
      editor.focus();
      if (andRun) run();
    },
    destroy() {
      dead = true;
      stopRequested = true;
      editor.destroy();
      params.destroy();
      if (tableCtl) tableCtl.destroy();
      if (running && jobId !== null) stop();
    },
  };
}
