import { state } from './core/state.js';
import { q, qa, showToast } from './core/dom.js';
import { apiCall } from './core/api.js';

const MAX_CONSOLE_ENTRIES = 3200;
const consoleEntries = [];
let consoleFilter = 'all';
let consoleKeyword = '';
let autoScroll = true;

function classifyLevel(text) {
  const s = String(text || '');
  if (s.includes('[ERROR]') || s.includes('失败') || s.includes('异常') || s.includes('错误')) return 'error';
  if (s.includes('[WARN]') || s.includes('警告') || s.includes('[前端')) return 'warn';
  if (s.trim().startsWith('>>> [状态]')) return 'status';
  return 'info';
}

function maybeToastStatusFromConsole(text) {
  const line = String(text || '').trim();
  if (!line.startsWith('>>> [状态]')) return;

  const mapping = [
    { key: 'stop', raw: '>>> [状态] 已停止录制', toast: '录制已停止（快捷键）' },
    { key: 'clear', raw: '>>> [状态] 已清空录制缓存', toast: '录制缓存已清空（快捷键）' },
    { key: 'arm', raw: '>>> [状态] 已布防', toast: '录制已布防（快捷键）' },
    { key: 'start', raw: '>>> [状态] 开始录制', toast: '录制已开始' },
  ];

  const hit = mapping.find((item) => line.startsWith(item.raw));
  if (!hit) return;

  const now = Date.now();
  const lastTs = Number(state.statusToastTs[hit.key] || 0);
  if (now - lastTs < 450) return;
  state.statusToastTs[hit.key] = now;
  showToast(hit.toast, 1800);
}

function matchesFilter(entry) {
  if (consoleFilter === 'status' && entry.level !== 'status') return false;
  if (consoleFilter === 'warn' && entry.level !== 'warn' && entry.level !== 'error') return false;
  if (consoleKeyword) {
    const needle = consoleKeyword.toLowerCase();
    return entry.text.toLowerCase().includes(needle) || entry.ts.toLowerCase().includes(needle);
  }
  return true;
}

function createLineNode(entry) {
  const row = document.createElement('div');
  row.className = `console-line level-${entry.level}`;

  const ts = document.createElement('span');
  ts.className = 'console-ts';
  ts.textContent = entry.ts;

  const msg = document.createElement('span');
  msg.className = 'console-msg';
  msg.textContent = entry.text;

  row.append(ts, msg);
  return row;
}

function updateLineBadge(visibleCount) {
  const badge = q('#consoleLineBadge');
  if (!badge) return;
  const total = consoleEntries.length;
  badge.textContent = visibleCount === total ? `${total} 行` : `${visibleCount} / ${total} 行`;
}

function renderAllConsole() {
  const host = q('#consoleOutput');
  if (!host) return;
  const frag = document.createDocumentFragment();
  let visible = 0;
  for (const entry of consoleEntries) {
    if (!matchesFilter(entry)) continue;
    frag.appendChild(createLineNode(entry));
    visible += 1;
  }
  host.replaceChildren(frag);
  if (!visible) {
    const empty = document.createElement('div');
    empty.className = 'console-empty';
    empty.textContent = consoleEntries.length ? '当前筛选条件下没有匹配日志' : '暂无运行日志，启动监听或执行宏后输出将显示在这里';
    host.appendChild(empty);
  }
  updateLineBadge(visible);
  if (autoScroll) host.scrollTop = host.scrollHeight;
}

export async function pollConsole() {
  if (state.pollConsoleBusy) return;
  state.pollConsoleBusy = true;
  try {
    const data = await apiCall('poll_console', state.consoleLastId, 120);
    const entries = data.entries || [];
    if (!entries.length) return;

    const host = q('#consoleOutput');
    const frag = document.createDocumentFragment();
    let addedVisible = 0;

    entries.forEach((item) => {
      const entry = {
        ts: String(item.ts || ''),
        text: String(item.text || ''),
        level: classifyLevel(item.text),
      };
      consoleEntries.push(entry);
      maybeToastStatusFromConsole(entry.text);
      if (matchesFilter(entry)) {
        frag.appendChild(createLineNode(entry));
        addedVisible += 1;
      }
    });

    state.consoleLastId = data.latest_id || state.consoleLastId;

    if (consoleEntries.length > MAX_CONSOLE_ENTRIES) {
      consoleEntries.splice(0, consoleEntries.length - 2600);
      renderAllConsole();
      return;
    }

    state.consoleLineCount = consoleEntries.length;
    if (host && addedVisible > 0) {
      host.querySelector('.console-empty')?.remove();
      host.appendChild(frag);
      while (host.children.length > MAX_CONSOLE_ENTRIES) {
        host.removeChild(host.firstElementChild);
      }
      if (autoScroll) host.scrollTop = host.scrollHeight;
    }
    updateLineBadge(host ? host.querySelectorAll('.console-line').length : 0);
  } catch (err) {
    console.error(err);
  } finally {
    state.pollConsoleBusy = false;
  }
}

export async function clearConsole() {
  await apiCall('clear_console');
  state.consoleLastId = 0;
  state.consoleLineCount = 0;
  consoleEntries.length = 0;
  renderAllConsole();
  showToast('控制台已清空');
}

export function bindConsoleActions() {
  renderAllConsole();

  q('#btnClearConsole')?.addEventListener('click', () => {
    clearConsole().catch((err) => showToast(err.message));
  });

  q('#btnCopyConsole')?.addEventListener('click', async () => {
    const lines = consoleEntries.filter(matchesFilter).map((e) => `[${e.ts}] ${e.text}`);
    if (!lines.length) return showToast('没有可复制的日志');
    try {
      await navigator.clipboard.writeText(lines.join('\n'));
      showToast(`已复制 ${lines.length} 行日志`);
    } catch {
      showToast('复制失败');
    }
  });

  q('#btnConsoleAutoScroll')?.addEventListener('click', () => {
    autoScroll = !autoScroll;
    const btn = q('#btnConsoleAutoScroll');
    if (btn) {
      btn.textContent = `自动滚底：${autoScroll ? '开' : '关'}`;
      btn.classList.toggle('active', autoScroll);
    }
    if (autoScroll) {
      const host = q('#consoleOutput');
      if (host) host.scrollTop = host.scrollHeight;
    }
  });

  qa('#consoleFilterSeg [data-console-filter]').forEach((btn) => {
    btn.addEventListener('click', () => {
      consoleFilter = btn.dataset.consoleFilter || 'all';
      qa('#consoleFilterSeg [data-console-filter]').forEach((b) => {
        b.classList.toggle('active', b === btn);
      });
      renderAllConsole();
    });
  });

  q('#consoleSearch')?.addEventListener('input', (e) => {
    consoleKeyword = e.target.value || '';
    renderAllConsole();
  });
}
