// 前端异常落盘：把 window.onerror / unhandledrejection 送去后端写 logs/frontend.log。
// 目的：DevTools 里那些「问题条数」关掉窗口就没了，留个能翻的记录。
const MAX_UNIQUE = 200;
const MAX_QUEUE = 50;

let installed = false;
const seen = new Set();
const queue = [];

function post(kind, message, source, stack, tag) {
  const text = String(message == null ? '' : message);
  const key = `${kind}|${text}|${source || ''}`;
  if (seen.has(key) || seen.size >= MAX_UNIQUE) return;
  seen.add(key);

  try {
    // 开发时也能直接在 DevTools 里看到
    console.error(`[${tag}][${kind}]`, text, source || '', stack || '');
  } catch (e) { /* ignore */ }

  const api = window.pywebview && window.pywebview.api;
  if (api && typeof api.log_frontend_error === 'function') {
    try {
      api.log_frontend_error(text, String(source || ''), String(stack || ''), kind).catch(() => {});
      return;
    } catch (e) { /* 落到队列里等 pywebview 就绪 */ }
  }
  if (queue.length < MAX_QUEUE) queue.push([kind, text, String(source || ''), String(stack || '')]);
}

function flush() {
  const api = window.pywebview && window.pywebview.api;
  if (!api || typeof api.log_frontend_error !== 'function') return;
  while (queue.length) {
    const [kind, message, source, stack] = queue.shift();
    try {
      api.log_frontend_error(message, source, stack, kind).catch(() => {});
    } catch (e) {
      queue.unshift([kind, message, source, stack]);
      return;
    }
  }
}

export function installErrorLogging(tag = 'webui') {
  if (installed) return;
  installed = true;

  window.addEventListener('error', (event) => {
    if (!event || !event.message) return;
    const stack = event.error && event.error.stack;
    post('error', event.message, `${event.filename || ''}:${event.lineno || 0}`, stack, tag);
  });

  window.addEventListener('unhandledrejection', (event) => {
    const reason = event && event.reason;
    post('rejection', (reason && reason.message) || String(reason), '', reason && reason.stack, tag);
  });

  window.addEventListener('pywebviewready', flush);
  // pywebview 可能已经就绪（本模块是后加载的）
  setTimeout(flush, 1500);
}
