import { openWindow } from "./tabs.js";

export function isTemplate(path) {
  return /\.(Template|Макет)\./.test(path || "") || /^(CommonTemplate|ОбщийМакет)\./.test(path || "");
}

export function openMetadata({ path, icon, module }) {
  return openWindow(module ? { kind: "module", path, icon, module } :
    { kind: isTemplate(path) ? "template" : "object", path, icon });
}
