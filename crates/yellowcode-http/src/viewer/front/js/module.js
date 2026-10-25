import { el, fill, clear } from "./dom.js";
import { api, show } from "./api.js";
import { T, modLabel } from "./i18n.js";
import { openWindow, registerKind, update, activeWindow, activate } from "./tabs.js";
import { openBuilder } from "./x/qb.js";
import { createEditor, releaseEditor, goToLine, cm } from "./editor.js";

const READ_ONLY = true;
let messageGeneration = 0;
const toolbarMatches = cm.ViewPlugin.fromClass(class {
  constructor(view) { this.decorations = this.mark(view); }
  update(update) { this.decorations = this.mark(update.view); }
  mark(view) {
    const query = cm.getSearchQuery(view.state);
    const marks = [];
    for (const selection of view.state.selection.ranges) {
      for (const visible of view.visibleRanges) {
        const from = Math.max(selection.from, visible.from), to = Math.min(selection.to, visible.to);
        if (from < to) marks.push(cm.Decoration.mark({ class: "cm-inverted" }).range(from, to));
      }
    }
    if (!query.valid) return cm.Decoration.set(marks, true);
    for (const { from, to } of view.visibleRanges) {
      const cursor = query.getCursor(view.state, from, to);
      while (!cursor.next().done) {
        const { from: start, to: end } = cursor.value;
        if (start < end) {
          const current = view.state.selection.ranges.some((r) => r.from === start && r.to === end);
          marks.push(cm.Decoration.mark({ class: "cm-searchMatch" + (current ? " cm-searchMatch-selected cm-inverted" : "") }).range(start, end));
        }
      }
    }
    return cm.Decoration.set(marks, true);
  }
}, { decorations: (plugin) => plugin.decorations });
function messages() {
  let panel = document.getElementById("messages");
  if (!panel) {
    panel = el("section", { id: "messages", "aria-label": "Service messages" },
      el("div", { class: "dock-caption" }, document.documentElement.lang === "en" ? "Service messages" : "Служебные сообщения",
        el("button", { type: "button", "aria-label": T.close, onclick: () => { panel.hidden = true; } }, el("span", { class: "chrome-glyph chrome-close", "aria-hidden": "true" }))),
      el("div", { class: "check-status" }), el("ul", { class: "check-list" }));
    document.getElementById("layout").after(panel);
  }
  panel.hidden = false;
  return panel;
}
function caret(view, win) {
  if (activeWindow() !== win) return;
  const pos = view.state.selection.main.head, line = view.state.doc.lineAt(pos);
  document.querySelector("#statusbar span:last-child").textContent = `CAP　 NUM　 OVR　 ru　 Стр: ${line.number}　 Кол: ${pos - line.from + 1}`;
}

function createModule(win) {
  let mod = null, view = null, seq = 0, statusTimer = 0, checking = false;
  const modsel = el("span", { class: "modsel" });
  const info = el("span", { class: "modinfo msg" });
  const status = el("span", { class: "modstatus msg" });
  const outline = el("div", { class: "outline" });
  const code = el("div", { class: "code cm-host" });
  win.pane.append(
    el("div", { class: "modbar" }, modsel, info, status),
    el("div", { class: "modbody" }, outline, el("div", { class: "codewrap" }, code)),
  );

  function foldRange(block, state) {
    const start = state.doc.line(Math.max(1, Math.min(block.start, state.doc.lines)));
    const end = state.doc.line(Math.max(1, Math.min(block.end, state.doc.lines)));
    return end.to > start.to ? { from: start.to, to: end.to } : null;
  }
  function goTo(line, col = 1) {
    if (!view || !mod) return;
    const target = view.state.doc.line(Math.max(1, Math.min(line, view.state.doc.lines))).from;
    const effects = [];
    cm.foldedRanges(view.state).between(0, view.state.doc.length, (from, to) => {
      if (from <= target && target <= to) effects.push(cm.unfoldEffect.of({ from, to }));
    });
    if (effects.length) view.dispatch({ effects });
    goToLine(view, line, col);
  }
  function say(text) {
    status.textContent = text;
    clearTimeout(statusTimer);
    statusTimer = setTimeout(() => { status.textContent = ""; }, 4000);
  }
  function gotoDefinition(line, col) {
    if (!mod) return;
    const mine = seq;
    api("definition", { path: win.path, module: mod.module, line, col }).then((loc) => {
      if (mine !== seq || win.closed) return;
      if (loc.path === win.path && loc.module === mod.module) goTo(loc.line, loc.col);
      else openWindow({ kind: "module", path: loc.path, module: loc.module, line: loc.line, icon: "module:" + loc.module });
    }, () => { if (mine === seq && !win.closed) say(T.noDefinition); });
  }
  function onKey(ev) {
    if (!view || win.closed || activeWindow() !== win) return;
    if (ev.key === "F12" && !ev.shiftKey && !ev.altKey && !ev.metaKey && !ev.ctrlKey) {
      ev.preventDefault();
      const pos = view.state.selection.main.head, line = view.state.doc.lineAt(pos);
      gotoDefinition(line.number, pos - line.from + 1);
    } else if ((ev.metaKey || ev.ctrlKey) && ev.key.toLowerCase() === "f") {
      ev.preventDefault();
      document.getElementById("search").focus();
    }
  }
  document.addEventListener("keydown", onKey);
  function collapseAll() {
    if (!view || !mod) return;
    view.dispatch({ effects: mod.blocks.map((b) => foldRange(b, view.state)).filter(Boolean).map((r) => cm.foldEffect.of(r)) });
  }

  function selectedBlock() {
    if (!view || !mod) return null;
    const line = view.state.doc.lineAt(view.state.selection.main.head).number;
    return mod.blocks.filter((b) => b.start <= line && b.end >= line)
      .sort((a, b) => (a.end - a.start) - (b.end - b.start))[0];
  }
  function foldBlock(nested) {
    const block = selectedBlock();
    if (!block) return;
    const blocks = nested ? mod.blocks.filter((b) => b.start >= block.start && b.end <= block.end) : [block];
    view.dispatch({ effects: blocks.map((b) => foldRange(b, view.state)).filter(Boolean).map((r) => cm.foldEffect.of(r)) });
  }
  function unfoldBlock(nested) {
    if (!view) return;
    const pos = view.state.selection.main.head, effects = [];
    const block = selectedBlock();
    const range = block && foldRange(block, view.state);
    cm.foldedRanges(view.state).between(0, view.state.doc.length, (from, to) => {
      if ((from <= pos && pos <= to) || view.state.doc.lineAt(from).number === view.state.doc.lineAt(pos).number ||
          (nested && range && from >= range.from && to <= range.to)) effects.push(cm.unfoldEffect.of({ from, to }));
    });
    view.dispatch({ effects });
  }

  function renderOutline(items) {
    fill(outline);
    for (const it of items) {
      const d = el("div", { "data-line": it.line, "data-name": it.name });
      if (it.directives && it.directives.length) d.appendChild(el("span", { class: "dir" }, it.directives.join(" ")));
      d.appendChild(el("span", null, it.kind + " " + it.name));
      if (it.export) d.appendChild(el("span", { class: "ex" }, " " + T.export));
      if (it.region) d.appendChild(el("span", { class: "reg" }, "  #" + it.region));
      d.tabIndex = 0;
      d.addEventListener("click", () => goTo(it.line));
      d.addEventListener("keydown", (e) => { if (e.key === "Enter") goTo(it.line); });
      outline.appendChild(d);
    }
  }

  function load(module, line) {
    const mine = ++seq;
    info.textContent = T.loading;
    info.className = "modinfo msg";
    mod = null;
    if (view) releaseEditor(view);
    view = null;
    clear(code);
    clear(outline);
    api("module", { path: win.path, module: module || null, plain: 1 }).then((d) => {
      if (mine !== seq || win.closed) return;
      const resolved = module || d.modules[0] || null;
      mod = { module: resolved, blocks: Array.isArray(d.blocks) ? d.blocks : [] };
      const ranges = new Map();
      for (const block of mod.blocks) {
        if (!ranges.has(block.start)) ranges.set(block.start, block);
      }
      view = createEditor({ parent: code, text: typeof d.text === "string" ? d.text : d.lines.map((l) => l.map((r) => r[1]).join("")).join("\n"), readOnly: READ_ONLY,
        extensions: [toolbarMatches,
          cm.foldService.of((state, from) => {
            const block = ranges.get(state.doc.lineAt(from).number);
            return block ? foldRange(block, state) : null;
          }), cm.foldGutter({ markerDOM: (open) => el("span", { title: open ? "Fold line" : "Unfold line" }, open ? "⊟" : "⊞") }), cm.lintGutter(),
          cm.codeFolding({ placeholderDOM(view, onclick, prepared) {
            const box = el("span", { class: "cm-foldPlaceholder", onclick }, prepared);
            return box;
          }, preparePlaceholder(state, range) {
            return state.doc.lineAt(range.from).text.trimStart().startsWith("//") ? "//..." : "...";
          } }),
          cm.EditorView.updateListener.of((u) => { if (u.selectionSet || u.focusChanged) caret(u.view, win); }),
          cm.EditorView.domEventHandlers({ click(ev, editor) {
            if (!ev.ctrlKey && !ev.metaKey) return false;
            const pos = editor.posAtCoords({ x: ev.clientX, y: ev.clientY });
            if (pos === null) return false;
            const line = editor.state.doc.lineAt(pos);
            gotoDefinition(line.number, pos - line.from + 1);
            ev.preventDefault();
            return true;
          } }),
        ],
      });
      const name = win.path.split(".").pop();
      const prefix = win.path.startsWith("CommonModule.") ? (document.documentElement.lang === "en" ? "Common module " : "Общий модуль ") : "";
      update(win, { module: resolved, title: prefix + name + ": " + modLabel(resolved) + (document.documentElement.lang === "en" ? " [Read only]" : " [Только для чтения]") });
      caret(view, win);
      for (const b of modsel.querySelectorAll("button")) b.classList.toggle("cur", b.getAttribute("data-module") === resolved);
      info.textContent = d.name + " — " + (d.line_count ?? d.lines.length) + " " + T.linesN + (d.syntax_error ? " — " + T.syntax + d.syntax_error : "");
      renderOutline(d.outline);
      document.dispatchEvent(new Event("commands-changed"));
      if (line) goTo(line);
    }, (e) => {
      if (mine !== seq || win.closed) return;
      info.textContent = T.error + e.message;
      info.className = "modinfo msg bad";
    });
  }
  function checkModule() {
    if (!mod || !view || checking) return;
    const mine = seq;
    const panel = messages(), generation = ++messageGeneration;
    const checkStatus = panel.querySelector(".check-status"), checkList = panel.querySelector(".check-list");
    clear(checkList);
    checkStatus.textContent = T.checking;
    checking = true;
    document.dispatchEvent(new Event("commands-changed"));
    api("check", { path: win.path, module: mod.module }).then((d) => {
      if (mine !== seq || !view || win.closed || generation !== messageGeneration) return;
      const diagnostics = d.diagnostics.map((x) => {
        const line = view.state.doc.line(Math.max(1, Math.min(x.line, view.state.doc.lines)));
        const from = Math.min(line.to, line.from + Math.max(0, x.col - 1));
        return { from, to: Math.min(line.to, from + 1), severity: "error", message: x.text };
      });
      view.dispatch(cm.setDiagnostics(view.state, diagnostics));
      checkStatus.textContent = d.diagnostics.length ? T.errors + d.diagnostics.length : (document.documentElement.lang === "en" ? "No syntax errors found!" : "Синтаксических ошибок не обнаружено!");
      checkStatus.className = "check-status" + (d.diagnostics.length ? " bad" : "");
      for (const x of d.diagnostics) checkList.appendChild(el("li", { class: "diag", "data-line": x.line, "data-col": x.col, ondblclick: () => { if (!win.closed) { activate(win); goTo(x.line, x.col); } } }, x.text));
      for (const w of d.warnings || []) checkList.appendChild(el("li", { class: "warn" }, w));
    }).catch((e) => { if (mine === seq && !win.closed && generation === messageGeneration) checkStatus.textContent = T.error + e.message; })
      .then(() => { checking = false; document.dispatchEvent(new Event("commands-changed")); });
  }

  show(win.path).then(
    (d) => {
      if (win.closed) return;
      for (const m of d.modules) {
        const b = el("button", {
          class: "btn", type: "button", "data-module": m,
          onclick: () => openWindow({ kind: "module", path: win.path, module: m, icon: "module:" + m }),
        }, modLabel(m));
        b.classList.toggle("cur", m === win.module);
        modsel.appendChild(b);
      }
      if (!d.modules.length) info.textContent = T.noModule;
    },
    () => {},
  );
  load(win.module, win.line);
  return { goTo, ready: () => !!view && !!mod,
    find: () => document.getElementById("search").focus(),
    setSearch: (search, regexp = false) => { if (view) view.dispatch({ effects: cm.setSearchQuery.of(new cm.SearchQuery({ search, regexp, caseSensitive: false })) }); },
    status: () => { if (view) caret(view, win); },
    findNext: () => { if (view && document.getElementById("search").value) cm.findNext(view); else document.getElementById("search").focus(); },
    findPrevious: () => { if (view && document.getElementById("search").value) cm.findPrevious(view); else document.getElementById("search").focus(); },
    foldAll: collapseAll, unfoldAll: () => { if (view) cm.unfoldAll(view); },
    fold: () => foldBlock(false), foldSub: () => foldBlock(true),
    unfold: () => unfoldBlock(false), unfoldSub: () => unfoldBlock(true),
    check: checkModule, canCheck: () => !checking,
    builder: () => openBuilder({ literals: { path: win.path, module: win.module } }),
    outline: () => {
      win.pane.classList.toggle("show-outline");
      const first = outline.querySelector("[data-line]");
      if (first) { first.tabIndex = 0; first.focus(); first.scrollIntoView({ block: "nearest" }); }
    },
    selectAll: () => { if (view) { view.dispatch({ selection: { anchor: 0, head: view.state.doc.length } }); view.focus(); } },
    copy: () => { if (view) return navigator.clipboard.writeText(view.state.sliceDoc(view.state.selection.main.from, view.state.selection.main.to)); },
    getText: () => view ? view.state.doc.toString() : "", destroy() { seq++; clearTimeout(statusTimer); if (view) releaseEditor(view); document.removeEventListener("keydown", onKey); } };
}

export function initModule() {
  registerKind("module", { create: createModule });
}
