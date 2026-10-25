export const PORTION = 500;

function pause(ms, signal) {
  return new Promise((resolve, reject) => {
    const abort = () => reject(new DOMException("aborted", "AbortError"));
    if (signal && signal.aborted) return abort();
    const onAbort = () => { clearTimeout(t); abort(); };
    const t = setTimeout(() => {
      if (signal) signal.removeEventListener("abort", onAbort);
      resolve();
    }, ms);
    if (signal) signal.addEventListener("abort", onAbort, { once: true });
  });
}

export function api(path, params, signal) {
  const q = [];
  for (const k of Object.keys(params || {})) {
    const v = params[k];
    if (v !== null && v !== undefined) q.push(encodeURIComponent(k) + "=" + encodeURIComponent(v));
  }
  const url = "/api/" + path + (q.length ? "?" + q.join("&") : "");
  const once = () => fetch(url, signal ? { signal } : undefined).then((r) =>
    r.json().then(
      (j) => {
        if (r.status === 503 && j && j.loading) return pause(j.retry_ms > 0 ? j.retry_ms : 1000, signal).then(once);
        if (!r.ok) throw new Error((j && j.error) || String(r.status));
        return j;
      },
      () => {
        throw new Error(String(r.status));
      },
    ),
  );
  return once();
}

const SHOW_CACHE_MAX = 64;
const showCache = new Map();

export function show(path) {
  const hit = showCache.get(path);
  if (hit) {
    showCache.delete(path);
    showCache.set(path, hit);
    return hit;
  }
  const p = api("show", { path });
  showCache.set(path, p);
  p.catch(() => showCache.delete(path));
  if (showCache.size > SHOW_CACHE_MAX) showCache.delete(showCache.keys().next().value);
  return p;
}

let iconVersion = "";

export function setIconVersion(v) {
  iconVersion = typeof v === "string" ? v : "";
}

export function iconUrl(key, extra) {
  const q = [extra, iconVersion ? "v=" + encodeURIComponent(iconVersion) : ""].filter(Boolean).join("&");
  if (!key.includes("/")) {
    const [head, tail] = key.split(":");
    const kind = key === "root" ? "Configuration" : key === "recalc" ? "Recalculation"
      : head === "module" || key === "folder:Modules" ? "CommonModule"
      : head === "folder" ? tail.replace(/s$/, "") : tail;
    key = "tree/" + kind;
  }
  return "/skin/icon/" + key.split("/").map(encodeURIComponent).join("/") + (q ? "?" + q : "");
}
