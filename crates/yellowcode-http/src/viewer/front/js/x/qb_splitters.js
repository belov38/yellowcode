import { el } from "../dom.js";
import { tx } from "./waiting.js";

function splitter(orientation, label, read, write) {
  const vertical = orientation === "vertical";
  const node = el("div", { class: "xb-pane-splitter", role: "separator", tabindex: "0", "aria-orientation": orientation,
    "aria-label": label, title: label, "aria-valuemin": "15", "aria-valuemax": "80" });
  const set = (value) => { const actual = write(value); node.setAttribute("aria-valuenow", String(Math.round(actual))); };
  node.addEventListener("pointerdown", (event) => {
    if (event.button !== 0) return;
    event.preventDefault();
    const start = read();
    const origin = vertical ? event.clientX : event.clientY;
    const move = (e) => set(start.value + ((vertical ? e.clientX : e.clientY) - origin) * 100 / start.length);
    const end = () => { node.removeEventListener("pointermove", move); node.removeEventListener("lostpointercapture", end); };
    node.setPointerCapture(event.pointerId);
    node.addEventListener("pointermove", move);
    node.addEventListener("lostpointercapture", end);
  });
  node.addEventListener("keydown", (event) => {
    const direction = event.key === (vertical ? "ArrowRight" : "ArrowDown") ? 1 : event.key === (vertical ? "ArrowLeft" : "ArrowUp") ? -1 : 0;
    if (!direction) return;
    event.preventDefault(); event.stopPropagation();
    set(read().value + direction * (event.shiftKey ? 5 : 1));
  });
  node.setAttribute("aria-valuenow", String(Math.round(read().value)));
  return node;
}

export function installFieldSplitters(root, tableTransfer, fieldTransfer, mid, sources) {
  const shares = [30, 30, 40];
  const minima = [15, 18, 24];
  const columns = [...root.children].filter((n) => n.classList.contains("xb-col"));
  const available = () => columns.reduce((width, col) => width + col.getBoundingClientRect().width, 0) || 700;
  const refresh = () => root.querySelectorAll('.xb-pane-splitter[aria-orientation="vertical"]').forEach((n, i) => {
    n.setAttribute("aria-valuenow", String(Math.round(shares[i])));
    n.setAttribute("aria-valuemin", String(minima[i]));
    n.setAttribute("aria-valuemax", String(Math.round(shares[i] + shares[i + 1] - minima[i + 1])));
  });
  const addColumn = (host, index, label) => {
    host.append(splitter("vertical", label, () => ({ value: shares[index], length: available() }), (value) => {
      const total = shares[index] + shares[index + 1];
      shares[index] = Math.max(minima[index], Math.min(total - minima[index + 1], value));
      shares[index + 1] = total - shares[index];
      shares.forEach((share, i) => root.style.setProperty("--xb-col-" + i, share + "fr"));
      refresh();
      return shares[index];
    }));
  };
  addColumn(tableTransfer, 0, tx("Ширина списка базы данных", "Database pane width"));
  addColumn(fieldTransfer, 1, tx("Ширина списка таблиц", "Tables pane width"));
  refresh();
  let sourceShare = 38;
  sources.after(splitter("horizontal", tx("Высота списка выбранных таблиц", "Selected tables pane height"),
    () => ({ value: sourceShare, length: mid.clientHeight }), (value) => {
      sourceShare = Math.max(15, Math.min(80, value));
      mid.style.setProperty("--xb-source-share", sourceShare + "%");
      return sourceShare;
    }));
}
