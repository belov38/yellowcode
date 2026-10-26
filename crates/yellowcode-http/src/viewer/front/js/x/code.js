import { createEditor, goToLine, cm } from "../editor.js";
import { el, fill, clear } from "../dom.js";
import { T } from "../i18n.js";
import { update } from "../tabs.js";
import { codeCompletion } from "./code_completion.js";
import { cget, cpost } from "./net.js";

const POLL_MS = 120;
const MAX_BODY = 256 * 1024;

let modeGroups = 0;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const fmtMs = (ms) => (ms < 1000 ? ms + " " + T.codeMs : (ms / 1000).toFixed(1) + " " + T.codeSec);

const STATUS = {
  обычное: ["ordinary", ""], ordinary: ["ordinary", ""],
  безстатуса: ["none", ""], withoutstatus: ["none", ""],
  информация: ["info", "ℹ"], information: ["info", "ℹ"],
  внимание: ["attention", "⚠"], attention: ["attention", "⚠"],
  важное: ["important", "!"], important: ["important", "!"],
  оченьважное: ["very", "‼"], veryimportant: ["very", "‼"],
};
const statusOf = (s) => STATUS[String(s).toLowerCase().replace(/\s+/g, "")] || ["other", "•"];

const OUTCOME = {
  done: () => T.codeDone,
  error: () => T.codeError,
  cancelled: () => T.codeCancelled,
  budget: () => T.codeBudget,
  busy: () => T.codeBusy,
  internal: () => T.codeInternal,
};

function cancelKeepalive(id) {
  try {
    fetch("/api/x/code/cancel?id=" + encodeURIComponent(id), {
      method: "POST", keepalive: true, headers: { "Content-Type": "application/json" }, body: "{}",
    }).catch(() => {});
  } catch (e) {
  }
}

export function createCodeTab(win) {
  const host = el("div", { class: "xq-host" }, el("div", { class: "msg" }, T.codeStarting));
  win.pane.appendChild(host);
  update(win, { title: T.codeConsole });
  let ctl = null;
  let closed = false;
  const failed = (e) => fill(host, el("div", { class: "msg bad" }, T.error + e.message));
  const open = () =>
    cget("info").then((info) => {
      if (closed) return;
      if (info.base === false) {
        return fill(host, el("div", { class: "xq-notbase" },
          el("div", { class: "xq-wait xq-big", "data-wait": "base" }, String(info.reason || ""))));
      }
      if (!info.ready) {
        return fill(host, el("div", { class: "xq-notbase" },
          el("div", { class: "msg bad", "data-wait": "open-failed" }, T.codeOpenFailed + String(info.error || "")),
          el("button", { class: "btn", type: "button", onclick: () => {
            fill(host, el("div", { class: "msg" }, T.codeStarting));
            cpost("reopen", {}).then(open, failed);
          } }, T.codeReopen)));
      }
      ctl = buildConsole(host, info);
    }, (e) => { if (!closed) failed(e); });
  open();
  return { destroy() { closed = true; if (ctl) ctl.destroy(); } };
}

function buildConsole(host, info) {
  let jobId = null;
  let running = false;
  let dead = false;
  let cancelRequested = false;

  const runBtn = el("button", { class: "btn xc-run", type: "button", title: "Ctrl+Enter", onclick: () => run() }, T.codeRun);
  const cancelBtn = el("button", { class: "btn xc-cancel", type: "button", disabled: true, onclick: () => cancel() }, T.codeCancel);
  const time = el("span", { class: "xc-time dim" });
  const group = "xc-mode-" + (++modeGroups);
  const radio = (value, label, on, hint) =>
    el("label", { title: hint }, el("input", { type: "radio", name: group, value, checked: on, disabled: !on, title: hint }), " " + label);
  const mode = el("span", { class: "xc-mode", role: "radiogroup", "aria-label": T.codeMode },
    radio("sandbox", T.codeSandbox, true, null), radio("real", T.codeReal, false, T.codeRealHint));
  const bar = el("div", { class: "xq-bar xc-bar" }, runBtn, cancelBtn, time, mode);

  const session = el("span", { class: "xc-session" });
  const showSession = (i) => {
    const bare = i.session === "bare";
    session.textContent = bare ? T.codeSessionBare + String(i.sessionReason || "") : T.codeSessionFull;
    session.classList.toggle("xc-bare", bare);
  };
  showSession(info);
  const statusLine = el("div", { class: "xc-statusline" }, session, el("span", { class: "xc-rights dim" }, T.codeRights),
    el("span", { class: "xc-completion-note dim" }, T.codeCompletionNote));

  const ed = el("div", { class: "xc-ed cm-host" });
  const editor = createEditor({ parent: ed,
    extensions: codeCompletion(),
    onChange() { clearMark(); },
    keys: [{ key: "Mod-Enter", run() { run(); return true; } }],
  });
  const getText = () => editor.state.doc.toString();
  function clearMark() { editor.dispatch(cm.setDiagnostics(editor.state, [])); }
  function mark(line) {
    if (!(line >= 1 && line <= editor.state.doc.lines)) return;
    const l = editor.state.doc.line(line);
    const from = l.from + (l.text.length - l.text.trimStart().length);
    const to = from < l.to ? l.to : Math.min(editor.state.doc.length, from + 1);
    editor.dispatch(cm.setDiagnostics(editor.state, [{ from, to, severity: "error", message: T.codeError }]));
    goToLine(editor, line, from - l.from + 1);
  }

  const out = el("div", { class: "xc-out", role: "log" });

  function where(module, line, inFragment) {
    if (!module && !line) return null;
    return (module || (inFragment ? T.codeFragment : T.codeExecute)) + ", " + T.codeLine + " " + line;
  }

  function errorBlock(e) {
    const chain = el("div", { class: "xc-err-chain" },
      (e.chain || []).map((c, i) => el("div", { class: "xq-err-line xc-err-line", style: "padding-left:" + i * 12 + "px" }, String(c))));
    const stack = e.stack || [];
    const fragmentFrame = stack.length && !stack[stack.length - 1].module ? stack.length - 1 : -1;
    const w = where(e.module, e.line,
      stack.length === 0 || (stack.length === 1 && fragmentFrame === 0 && stack[0].line === e.line));
    return el("div", { class: "xq-err xc-err", role: "alert" },
      chain,
      w ? el("div", { class: "xc-err-where" }, w) : null,
      e.sourceLine ? el("pre", { class: "xc-err-src" }, String(e.sourceLine)) : null,
      stack.length ? el("div", { class: "xc-err-stack" },
        el("div", { class: "xc-err-head" }, T.codeStack),
        stack.map((f, i) => {
          const inFragment = i === fragmentFrame;
          return el("div", {
            class: "xc-frame" + (inFragment ? " xc-frame-go" : ""),
            role: inFragment ? "button" : null, tabindex: inFragment ? "0" : null,
            onclick: inFragment ? () => goToLine(editor, f.line) : null,
            onkeydown: inFragment ? (ev) => { if (ev.key === "Enter") goToLine(editor, f.line); } : null,
          }, el("span", { class: "xc-fr-where" }, where(f.module, f.line, inFragment) || ""),
          f.sourceLine ? el("span", { class: "xc-fr-src" }, String(f.sourceLine).trim()) : null);
        })) : null);
  }

  function showReport(rep) {
    clear(out);
    const outcome = OUTCOME[rep.outcome] ? rep.outcome : "error";
    out.appendChild(el("div", { class: "xc-outcome xc-o-" + outcome, "data-outcome": outcome }, OUTCOME[outcome]()));
    const msgs = rep.messages || [];
    const list = el("div", { class: "xc-msgs" });
    const frag = document.createDocumentFragment();
    for (const m of msgs) {
      const has = m.status !== null && m.status !== undefined;
      const [cls, icon] = has ? statusOf(m.status) : ["plain", ""];
      frag.appendChild(el("div", { class: "xc-msg xc-st-" + cls, "data-status": has ? String(m.status) : null, title: has ? String(m.status) : null },
        el("span", { class: "xc-msg-icon", "aria-hidden": "true" }, icon),
        el("span", { class: "xc-msg-text" }, String(m.text))));
    }
    list.appendChild(frag);
    if (!msgs.length && !rep.dropped) list.appendChild(el("div", { class: "dim xc-nomsg" }, T.codeNoMessages));
    out.appendChild(list);
    if (rep.dropped > 0) out.appendChild(el("div", { class: "xc-dropped" }, T.codeDropped + rep.dropped));
    const e = rep.error;
    const positioned = e && (e.line > 0 || (e.stack && e.stack.length) || e.at);
    if (e && (outcome === "error" || positioned)) out.appendChild(errorBlock(e));
    if (e && e.at) mark(e.at);
  }

  function plainError(text) {
    clear(out);
    out.appendChild(el("div", { class: "xq-err xc-err", role: "alert" }, el("div", { class: "xq-err-line" }, text)));
  }

  function setRunning(on) {
    running = on;
    runBtn.disabled = on;
    cancelBtn.disabled = !on;
  }

  async function run() {
    if (running || dead) return;
    const text = getText();
    if (!text.trim()) return;
    const size = new TextEncoder().encode(JSON.stringify({ text })).length;
    if (size > MAX_BODY) {
      clearMark();
      time.textContent = "";
      plainError(T.codeTooBig.replace("%n", Math.ceil(size / 1024)).replace("%m", MAX_BODY / 1024));
      return;
    }
    setRunning(true);
    jobId = null;
    cancelRequested = false;
    clear(out);
    clearMark();
    const startedAt = performance.now();
    time.textContent = T.codeRunning;
    const tick = setInterval(() => {
      time.textContent = T.codeRunning + " " + fmtMs(Math.round(performance.now() - startedAt));
    }, 200);
    let job = null;
    try {
      const { id } = await cpost("run", { text });
      jobId = id;
      if (cancelRequested) cpost("cancel", {}, { id }).catch(() => {});
      for (;;) {
        job = await cget("job", { id });
        if (job.state !== "running" || dead) break;
        await sleep(POLL_MS);
      }
    } catch (e) {
      if (jobId !== null) cpost("cancel", {}, { id: jobId }).catch(() => {});
      if (!dead) plainError(T.error + e.message);
    }
    clearInterval(tick);
    if (dead) return;
    setRunning(false);
    jobId = null;
    if (!job || !job.report) {
      time.textContent = "";
      return;
    }
    time.textContent = fmtMs(job.report.elapsedMs ?? job.elapsedMs ?? 0);
    showReport(job.report);
    if (job.report.outcome === "internal") cget("info").then((i) => { if (!dead && i.ready) showSession(i); }, () => {});
  }

  function cancel() {
    if (!running) return;
    if (jobId === null) {
      cancelRequested = true;
      return;
    }
    cpost("cancel", {}, { id: jobId }).catch(() => {});
  }

  const onPageHide = () => { if (running && jobId !== null) cancelKeepalive(jobId); };
  window.addEventListener("pagehide", onPageHide);

  fill(host, el("div", { class: "xq xc" }, bar, statusLine, ed, out));
  editor.focus();

  return {
    destroy() {
      dead = true;
      cancelRequested = true;
      window.removeEventListener("pagehide", onPageHide);
      if (running && jobId !== null) cancelKeepalive(jobId);
      editor.destroy();
    },
  };
}
