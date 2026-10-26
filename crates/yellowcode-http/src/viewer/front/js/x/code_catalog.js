export function createCatalog(fetcher, limit = 64) {
  const cache = new Map();
  return async (params, signal) => {
    const aborted = () => { if (signal?.aborted) throw new DOMException('aborted', 'AbortError'); };
    aborted();
    const key = JSON.stringify(params);
    if (cache.has(key)) {
      const hit = cache.get(key); cache.delete(key); cache.set(key, hit); return hit;
    }
    const result = await fetcher(params, signal);
    aborted();
    cache.set(key, result);
    if (cache.size > limit) cache.delete(cache.keys().next().value);
    return result;
  };
}
