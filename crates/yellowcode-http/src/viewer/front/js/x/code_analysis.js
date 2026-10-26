let nextDocument = 0;
export function createAnalysis(fetcher, documentId = 'console-' + Date.now().toString(36) + '-' + (++nextDocument)) {
  let revision = 0, serial = 0, dead = false;
  const pending = new Map();
  const invalidate = (edited = false) => {
    if (edited) revision++;
    for (const request of pending.values()) request.abort.abort();
    pending.clear();
  };
  return {
    invalidate,
    destroy() { dead = true; invalidate(); },
    async request(input, signal) {
      if (dead || signal?.aborted) return null;
      const body = {...input, documentId, revision, requestId: ++serial};
      if (new TextEncoder().encode(JSON.stringify(body)).byteLength > 256 * 1024) {
        throw new RangeError('Analysis request exceeds 256 KiB');
      }
      pending.get(input.kind)?.abort.abort();
      const request = {abort: new AbortController()};
      pending.set(input.kind, request);
      const cancel = () => request.abort.abort();
      signal?.addEventListener('abort', cancel, {once: true});
      try {
        const result = await fetcher(body, request.abort.signal);
        if (dead || request.abort.signal.aborted || pending.get(input.kind) !== request ||
            revision !== body.revision || result.documentId !== documentId ||
            result.revision !== body.revision || result.requestId !== body.requestId) return null;
        return result;
      } finally {
        signal?.removeEventListener('abort', cancel);
        if (pending.get(input.kind) === request) pending.delete(input.kind);
      }
    },
  };
}
