import { cm } from '../editor.js';
import { bslKeywords } from '../bsl-lang.js';
import { cpost } from './net.js';
import { analysisContext, callContext } from './code_context.js';
import { createAnalysis } from './code_analysis.js';

const langOf = (context) => /^[a-z_]/i.test(context?.languageHint || context?.prefix || context?.name || context?.path?.[0] || '') ? 'en' : 'ru';
const signatureText = (name, s) => name + '(' + s.parameters.join(', ') + ')';

function signatureDom(item, argument) {
  const dom = document.createElement('div');
  dom.className = 'xc-signature'; dom.setAttribute('role', 'tooltip');
  for (const s of item.signatures) {
    const row = document.createElement('div');
    row.appendChild(document.createTextNode(item.name + '('));
    s.parameters.forEach((p, i) => {
      if (i) row.appendChild(document.createTextNode(', '));
      const active = i === argument || (s.variadic && i === s.parameters.length - 1 && argument >= i);
      const part = document.createElement(active ? 'strong' : 'span');
      part.textContent = p;
      if (active) part.setAttribute('aria-current', 'true');
      row.appendChild(part);
    });
    row.appendChild(document.createTextNode(')')); dom.appendChild(row);
  }
  if (item.note) {
    const note = document.createElement('div'); note.textContent = item.note; dom.appendChild(note);
  }
  return dom;
}

export function codeCompletion() {
  const analysis = createAnalysis((body, signal) => cpost('analyze', body, undefined, signal));
  const source = async (context) => {
    const text = context.state.doc.toString();
    const c = analysisContext(text, context.pos);
    if (!c || (!context.explicit && !c.prefix && !c.member && !c.constructors)) return null;
    const abort = new AbortController();
    context.addEventListener('abort', () => abort.abort(), {onDocChange: true});
    try {
      const data = await analysis.request({text, positionUtf16: context.pos, kind: 'completion', lang: langOf(c)}, abort.signal);
      if (!data || context.aborted) return null;
      const options = data.items.map((item) => ({
        label: item.name, type: item.kind,
        detail: item.name_other !== item.name ? item.name_other : undefined,
        info: item.note || item.signatures.length ? () => {
          const dom = document.createElement('div');
          dom.textContent = [...item.signatures.map((s) => signatureText(item.name, s)), item.note].filter(Boolean).join('\n');
          return dom;
        } : undefined,
      }));
      if (!c.member && !c.constructors) {
        for (const keyword of bslKeywords) {
          if (keyword.startsWith(c.prefix.toLowerCase())) options.push({label: keyword, type: 'keyword'});
        }
      }
      return {from: data.from, to: data.to, options};
    } catch (_) { return null; }
  };

  const changed = cm.StateEffect.define();
  const signature = cm.StateField.define({
    create: () => null,
    update(value, tr) {
      if (tr.docChanged || tr.selection) value = null;
      for (const effect of tr.effects) if (effect.is(changed)) value = effect.value;
      return value;
    },
    provide: (field) => cm.showTooltip.from(field),
  });
  const plugin = cm.ViewPlugin.fromClass(class {
    constructor(view) { this.view = view; this.serial = 0; this.dead = false; this.schedule(); }
    update(update) {
      if (update.docChanged || update.selectionSet || update.focusChanged) {
        analysis.invalidate(update.docChanged); this.schedule();
      }
    }
    schedule() {
      clearTimeout(this.timer); this.abort?.abort();
      const serial = ++this.serial;
      this.timer = setTimeout(() => this.refresh(serial), 90);
    }
    async refresh(serial) {
      if (this.dead) return;
      const view = this.view;
      const pos = view.state.selection.main.head;
      const c = view.hasFocus ? callContext(view.state.doc.toString(), pos) : null;
      let tooltip = null;
      if (view.hasFocus) {
        this.abort = new AbortController();
        try {
          const data = await analysis.request({text: view.state.doc.toString(), positionUtf16: pos, kind: 'signature', lang: langOf(c)}, this.abort.signal);
          const item = data?.items.find((i) => i.signatures.length);
          if (item && data.anchor !== null && data.argument !== null) tooltip = {
            pos: data.anchor, above: true, strictSide: true,
            create: () => ({dom: signatureDom(item, data.argument)}),
          };
        } catch (_) {}
      }
      if (!this.dead && serial === this.serial && view.dom.isConnected) view.dispatch({effects: changed.of(tooltip)});
    }
    destroy() { analysis.destroy(); this.dead = true; ++this.serial; clearTimeout(this.timer); this.abort?.abort(); }
  });
  return [
    cm.autocompletion({override: [source], defaultKeymap: false, activateOnTyping: true, maxRenderedOptions: 60}),
    cm.Prec.highest(cm.keymap.of(cm.completionKeymap)),
    signature, plugin,
    cm.EditorView.baseTheme({
      '.cm-tooltip': {backgroundColor: 'var(--cfg-surface-field, #fff)', color: 'var(--cfg-text-field, #000)', border: '1px solid var(--cfg-border-control, #b3ac86)'},
      '.cm-tooltip-autocomplete > ul > li[aria-selected]': {backgroundColor: 'var(--cfg-selection-bg, #536ac2)', color: 'var(--cfg-text-selected, #fff)'},
      '.cm-completionInfo': {whiteSpace: 'pre-wrap'},
      '.xc-signature': {padding: '5px 8px', maxWidth: 'min(650px, 85vw)', whiteSpace: 'pre-wrap', overflowWrap: 'anywhere'},
      '.xc-signature strong': {textDecoration: 'underline', fontWeight: 'bold'},
    }),
  ];
}
