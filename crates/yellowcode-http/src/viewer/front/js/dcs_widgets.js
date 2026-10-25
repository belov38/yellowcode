import { el } from "./dom.js";

export function browseRows(host) {
  const rows = () => [...host.querySelectorAll('[data-browse-row]')].filter((r) => !r.hidden && !r.closest('[role=group][hidden]'));
  const select = (row, focus = false) => {
    host.querySelectorAll('[data-browse-row]').forEach((r) => { r.classList.toggle('selected', r === row); r.setAttribute('aria-selected', String(r === row)); r.tabIndex = r === row ? 0 : -1; });
    if (focus) row.focus({ preventScroll: true });
  };
  host.addEventListener('click', (e) => { const row = e.target.closest('[data-browse-row]'); if (row) select(row); });
  host.addEventListener('keydown', (e) => {
    const list = rows(); const row = e.target.closest('[data-browse-row]'); const i = list.indexOf(row);
    if (i < 0 || !['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(e.key)) return;
    e.preventDefault(); e.stopPropagation();
    const next = list[e.key === 'Home' ? 0 : e.key === 'End' ? list.length - 1 : Math.max(0, Math.min(list.length - 1, i + (e.key === 'ArrowDown' ? 1 : -1)))];
    select(next, true); next.click(); next.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  });
  if (rows()[0]) select(rows()[0]);
}

export function splitPane(parent, before, after, orientation, label, initial = 40, minima = null) {
  const vertical = orientation === 'vertical';
  parent.classList.add(vertical ? 'dcs-split-columns' : 'dcs-split-rows');
  let share = initial;
  const bar = el('div', { class: 'dcs-splitter', role: 'separator', tabindex: '0', 'aria-label': label,
    'aria-orientation': orientation, 'aria-valuemin': '18', 'aria-valuemax': '75' });
  const update = (value) => {
    const size = vertical ? parent.clientWidth : parent.clientHeight;
    let min = 18, max = 75;
    if (minima && size > 6) {
      const available = size - 6;
      const scale = Math.min(1, available / (minima.before + minima.after));
      min = Math.max(min, Math.ceil(minima.before * scale * 100 / size));
      max = Math.min(max, Math.floor((available - minima.after * scale) * 100 / size));
      if (min > max) min = max = Math.round(available * minima.before / (minima.before + minima.after) * 100 / size);
    }
    share = Math.max(min, Math.min(max, value));
    bar.setAttribute('aria-valuemin', String(min));
    bar.setAttribute('aria-valuemax', String(max));
    parent.style.setProperty('--dcs-share', share + '%');
    bar.setAttribute('aria-valuenow', String(Math.round(share)));
  };
  bar.addEventListener('pointerdown', (e) => {
    if (e.button !== 0) return;
    e.preventDefault(); bar.setPointerCapture(e.pointerId);
    const origin = vertical ? e.clientX : e.clientY; const start = share;
    const size = vertical ? parent.clientWidth : parent.clientHeight;
    const move = (ev) => update(start + ((vertical ? ev.clientX : ev.clientY) - origin) * 100 / Math.max(1, size));
    const end = () => { bar.removeEventListener('pointermove', move); bar.removeEventListener('lostpointercapture', end); };
    bar.addEventListener('pointermove', move); bar.addEventListener('lostpointercapture', end);
  });
  bar.addEventListener('pointerup', (e) => { if (bar.hasPointerCapture(e.pointerId)) bar.releasePointerCapture(e.pointerId); });
  bar.addEventListener('keydown', (e) => {
    const direction = e.key === (vertical ? 'ArrowRight' : 'ArrowDown') ? 1 : e.key === (vertical ? 'ArrowLeft' : 'ArrowUp') ? -1 : 0;
    if (!direction) return;
    e.preventDefault(); e.stopPropagation(); update(share + direction * (e.shiftKey ? 5 : 1));
  });
  parent.append(before, bar, after); update(initial);
  const observer = minima ? new ResizeObserver(() => update(share)) : null;
  observer?.observe(parent);
  return () => observer?.disconnect();
}

export function valueText(value) {
  if (value == null) return '';
  if (value.type === 'Null') return document.documentElement.lang === 'en' ? '<Empty value>' : '<Пустое значение>';
  if (/boolean$/i.test(value.type || '') && ['true', 'false'].includes(value.text)) return document.documentElement.lang === 'en' ? value.text : value.text === 'true' ? 'Истина' : 'Ложь';
  if (value.items?.length) return value.items.map(valueText).join('; ');
  return value.text || (value.type ? '[' + value.type + ']' : '');
}

export function usage(value) { return value === true ? '✓' : value === false ? '—' : ''; }
