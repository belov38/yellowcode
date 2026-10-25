import * as cm from "../vendor/codemirror.js";
import { bsl, query } from "./bsl-lang.js";
import { T } from "./i18n.js";
import { api } from "./api.js";

let stylePromise;
let showLineNumbers = false;
const lineSettings = new Map();

export function setLineNumbers(enabled) {
  showLineNumbers = !!enabled;
  for (const [view, compartment] of lineSettings) {
    if (view.dom.isConnected) view.dispatch({ effects: compartment.reconfigure(showLineNumbers ? cm.lineNumbers() : []) });
    else lineSettings.delete(view);
  }
}
export function editorSettings() { return { lineNumbers: showLineNumbers }; }
export function releaseEditor(view) { lineSettings.delete(view); view.destroy(); }
export function loadEditorStyle() {
  if (stylePromise) return stylePromise;
  stylePromise = api("editor").then((d) => {
    const style = document.documentElement.style;
    const names = { Keywords: "kw", Strings: "str", Numerics: "num", Dates: "date", Operators: "op", Comments: "com", Preprocessor: "pre", Others: "others", Identifiers: "ident", Background: "bg", PairLexeme: "pair", SearchResult: "search" };
    for (const [category, name] of Object.entries(names)) {
      const color = d.colors && d.colors[category];
      if (typeof color === "string" && /^#[\da-f]{6}$/i.test(color)) {
        style.setProperty("--ed-" + name, color);
        const inverted = (0xffffff ^ parseInt(color.slice(1), 16)).toString(16).padStart(6, "0");
        style.setProperty("--ed-inv-" + name, "#" + inverted);
      }
    }
    return d;
  }).catch(() => { stylePromise = null; });
  return stylePromise;
}

const colors = cm.HighlightStyle.define([
  { tag: cm.tags.keyword, class: "kw" }, { tag: cm.tags.string, class: "str" },
  { tag: cm.tags.number, class: "num" }, { tag: cm.tags.comment, class: "com" },
  { tag: cm.tags.variableName, class: "ident" }, { tag: cm.tags.operator, class: "op" },
  { tag: cm.tags.literal, class: "date" },
  { tag: cm.tags.meta, class: "pre" }, { tag: cm.tags.annotation, class: "ann" },
]);

function language(spec) {
  return { ...spec, tokenTable: Object.fromEntries(Object.entries(spec.tokenTable).map(([name, tag]) => [name, cm.tags[tag]])) };
}

export function createEditor({ parent, text = "", readOnly = false, queryLanguage = false, keys = [], extensions = [], onChange }) {
  const tabSize = new cm.Compartment();
  const numbers = new cm.Compartment();
  const view = new cm.EditorView({ parent, state: cm.EditorState.create({
    doc: text,
    extensions: [
      cm.EditorState.phrases.of(T.cmPhrases),
      cm.ViewPlugin.define((view) => ({ destroy() { lineSettings.delete(view); } })),
      cm.EditorState.readOnly.of(readOnly), tabSize.of(cm.EditorState.tabSize.of(4)),
      cm.EditorView.contentAttributes.of({ "aria-label": queryLanguage ? "Query text" : "Module text", spellcheck: "false" }),
      numbers.of(showLineNumbers ? cm.lineNumbers() : []), cm.drawSelection(),
      cm.StreamLanguage.define(language(queryLanguage ? query : bsl)), cm.syntaxHighlighting(colors),
      cm.search({ top: true }), cm.history(),
      cm.keymap.of([...keys, ...(readOnly ? [] : cm.searchKeymap),...cm.defaultKeymap, ...cm.historyKeymap, cm.indentWithTab]),
      cm.EditorView.updateListener.of((u) => { if (u.docChanged && onChange) onChange(u.state.doc.toString()); }),
      ...extensions,
    ],
  }) });
  parent.editorView = view;
  lineSettings.set(view, numbers);
  loadEditorStyle().then((d) => {
    if (d && view.dom.isConnected && Number.isInteger(d.tab_size) && d.tab_size > 0 && d.tab_size <= 32)
      view.dispatch({ effects: tabSize.reconfigure(cm.EditorState.tabSize.of(d.tab_size)) });
  });
  return view;
}

export function replaceText(view, text) {
  view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: text } });
}
export function goToLine(view, line, col = 1) {
  const l = view.state.doc.line(Math.max(1, Math.min(line, view.state.doc.lines)));
  const pos = Math.min(l.to, l.from + Math.max(0, col - 1));
  view.dispatch({ selection: { anchor: pos }, effects: cm.EditorView.scrollIntoView(pos, { y: "center" }) });
  view.focus();
}
export { cm };
