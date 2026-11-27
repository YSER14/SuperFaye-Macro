/* 富回复渲染：Markdown 子集 + 结构化 UI 组件块。
 *
 * 模型回复的 message 字段会带 Markdown（标题、列表、表格、代码块），
 * 关键结论可以再包一个 ```ui 组件块，内容是一段 JSON：
 *
 *   ```ui
 *   {"type":"callout","tone":"warning","content":"这一段会覆盖已保存的宏。"}
 *   ```
 *
 * 这里只做两件事：把纯文本拆成块，再把块渲染成 DOM。
 * 所有模型文本一律走 textContent，不拼 HTML 字符串，避免回复内容注入页面。
 */

const MAX_DEPTH = 4;
const RICH_FENCE = /^(?:ui|rich|component|components)$/i;
const SAFE_SCHEMES = new Set(['http:', 'https:', 'mailto:']);

const FENCE_RE = /^[ \t]*(`{3,}|~{3,})[ \t]*([^\s`]*)[ \t]*$/;
const HEADING_RE = /^[ \t]{0,3}(#{1,6})[ \t]+(.*)$/;
const HR_RE = /^[ \t]*(?:-{3,}|\*{3,}|_{3,})[ \t]*$/;
const ITEM_RE = /^([ \t]*)([-*+]|\d{1,9}[.)])[ \t]+(.*)$/;
const QUOTE_RE = /^[ \t]*>[ \t]?(.*)$/;
const TABLE_SEP_RE = /^[ \t]*\|?[ \t]*:?-{2,}:?[ \t]*(?:\|[ \t]*:?-{2,}:?[ \t]*)+\|?[ \t]*$/;
const TASK_RE = /^\[([ xX])\][ \t]+(.*)$/;
const ALERT_RE = /^\[!(note|tip|important|warning|caution|info|success|error|danger)\][ \t]*(.*)$/i;
/* 旧回复习惯用【小标题】分段，没有 # 也当成标题，历史对话才不至于糊成一段。 */
const LEGACY_HEADING_RE = /^【([^】\n]{1,24})】[ \t]*(.*)$/;

const CALLOUT_TONES = new Set(['info', 'success', 'warning', 'error']);
const TONE_ALIASES = {
  note: 'info', tip: 'info', info: 'info', neutral: 'info',
  ok: 'success', done: 'success', success: 'success', positive: 'success',
  warn: 'warning', important: 'warning', caution: 'warning', warning: 'warning',
  error: 'error', danger: 'error', critical: 'error', fail: 'error', negative: 'error',
};

function indentOf(line) {
  return line.replace(/\t/g, '    ').match(/^ */)[0].length;
}

function el(tag, className = '', text = '') {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== '') node.textContent = text;
  return node;
}

function isTableSep(line) {
  return typeof line === 'string' && line.includes('-') && TABLE_SEP_RE.test(line);
}

function startsBlock(line, next) {
  return FENCE_RE.test(line) || HEADING_RE.test(line) || HR_RE.test(line) || ITEM_RE.test(line)
    || QUOTE_RE.test(line) || LEGACY_HEADING_RE.test(line)
    || (line.includes('|') && isTableSep(next));
}

function safeHref(value) {
  const raw = String(value || '').trim();
  if (!/^(?:https?:\/\/|mailto:)/i.test(raw)) return null;
  try {
    const url = new URL(raw);
    return SAFE_SCHEMES.has(url.protocol) ? url.href : null;
  } catch {
    return null;
  }
}

/* ── 行内解析 ─────────────────────────────────────────────────────────── */

const ESCAPABLE = /[\\`*_{}[\]()#+\-.!~=|>]/;
const AUTOLINK_RE = /^https?:\/\/[^\s<>()"'`]+/;

function isBorder(ch) {
  return ch === ' ' || ch === '　' || ch === '\n' || ch === '\t';
}

export function parseInline(text, depth = 0) {
  const source = String(text ?? '');
  const tokens = [];
  let buffer = '';
  let i = 0;
  const flush = () => {
    if (buffer) {
      tokens.push({ t: 'text', v: buffer });
      buffer = '';
    }
  };
  const push = (token) => {
    flush();
    tokens.push(token);
  };
  while (i < source.length) {
    const ch = source[i];
    if (ch === '\n') {
      push({ t: 'br' });
      i += 1;
      continue;
    }
    if (ch === '\\' && ESCAPABLE.test(source[i + 1] || '')) {
      buffer += source[i + 1];
      i += 2;
      continue;
    }
    if (ch === '`') {
      const end = source.indexOf('`', i + 1);
      if (end > i + 1) {
        push({ t: 'code', v: source.slice(i + 1, end) });
        i = end + 1;
        continue;
      }
    }
    if (depth < MAX_DEPTH) {
      let handled = false;
      for (const [marker, kind] of [['**', 'strong'], ['__', 'strong'], ['~~', 'del'], ['==', 'mark']]) {
        if (!source.startsWith(marker, i)) continue;
        const end = source.indexOf(marker, i + marker.length);
        if (end > i + marker.length && !isBorder(source[i + marker.length])) {
          push({ t: kind, v: parseInline(source.slice(i + marker.length, end), depth + 1) });
          i = end + marker.length;
        } else {
          buffer += marker; // 未闭合的标记按字面文本处理
          i += marker.length;
        }
        handled = true;
        break;
      }
      if (handled) continue;
      if (source[i] === '[') {
        const match = /^\[([^\]\n]*)\]\(([^()\s]+)\)/.exec(source.slice(i));
        if (match) {
          const href = safeHref(match[2]);
          push(href
            ? { t: 'link', href, v: parseInline(match[1], depth + 1) }
            : { t: 'text', v: match[1] || match[2] });
          i += match[0].length;
          continue;
        }
      }
      const canEmphasize = ch === '*' || (ch === '_' && (i === 0 || isBorder(source[i - 1])));
      if (canEmphasize && !isBorder(source[i + 1] || '')) {
        const end = source.indexOf(ch, i + 1);
        if (end > i + 1 && source[end - 1] !== ' ' && !/[0-9]/.test(source[i - 1] || '')) {
          push({ t: 'em', v: parseInline(source.slice(i + 1, end), depth + 1) });
          i = end + 1;
          continue;
        }
      }
    }
    if (ch === 'h' && source.startsWith('http', i)) {
      const match = AUTOLINK_RE.exec(source.slice(i));
      if (match) {
        const href = safeHref(match[0]);
        if (href) {
          push({ t: 'link', href, v: [{ t: 'text', v: match[0] }] });
          i += match[0].length;
          continue;
        }
      }
    }
    buffer += ch;
    i += 1;
  }
  flush();
  return tokens;
}

function renderInline(tokens) {
  const fragment = document.createDocumentFragment();
  for (const token of tokens) {
    switch (token.t) {
      case 'text':
        fragment.append(document.createTextNode(token.v));
        break;
      case 'br':
        fragment.append(document.createElement('br'));
        break;
      case 'code':
        fragment.append(el('code', 'ai-rich-inline-code', token.v));
        break;
      case 'strong': {
        const node = el('strong');
        node.append(renderInline(token.v));
        fragment.append(node);
        break;
      }
      case 'em': {
        const node = el('em');
        node.append(renderInline(token.v));
        fragment.append(node);
        break;
      }
      case 'del': {
        const node = el('del');
        node.append(renderInline(token.v));
        fragment.append(node);
        break;
      }
      case 'mark': {
        const node = el('mark', 'ai-rich-mark');
        node.append(renderInline(token.v));
        fragment.append(node);
        break;
      }
      case 'link': {
        const node = el('a', 'ai-rich-link');
        node.href = token.href;
        node.target = '_blank';
        node.rel = 'noreferrer noopener';
        node.append(renderInline(token.v));
        fragment.append(node);
        break;
      }
      default:
        fragment.append(document.createTextNode(token.v || ''));
    }
  }
  return fragment;
}

function inlineNodes(value) {
  const fragment = document.createDocumentFragment();
  const text = String(value ?? '').split('\n').map((part) => part.trim()).join(' ').trim();
  fragment.append(renderInline(parseInline(text)));
  return fragment;
}

/* ── 块解析 ───────────────────────────────────────────────────────────── */

export function parseRichReply(source, depth = 0) {
  const lines = String(source ?? '').replace(/\r\n?/g, '\n').split('\n');
  return parseBlocks(lines, depth);
}

function parseBlocks(lines, depth = 0) {
  const blocks = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (!line.trim()) {
      i += 1;
      continue;
    }
    const fence = FENCE_RE.exec(line);
    if (fence) {
      const parsed = parseFence(lines, i);
      blocks.push(parsed.block);
      i = parsed.next;
      continue;
    }
    const heading = HEADING_RE.exec(line);
    if (heading) {
      blocks.push({ type: 'heading', level: heading[1].length, inlines: parseInline(heading[2].trim()) });
      i += 1;
      continue;
    }
    if (HR_RE.test(line)) {
      blocks.push({ type: 'hr' });
      i += 1;
      continue;
    }
    if (ITEM_RE.test(line) && !HR_RE.test(line)) {
      const list = parseList(lines, i, depth);
      blocks.push(list.block);
      i = list.next;
      continue;
    }
    if (QUOTE_RE.test(line)) {
      const parsed = parseQuote(lines, i, depth);
      blocks.push(parsed.block);
      i = parsed.next;
      continue;
    }
    if (line.includes('|') && isTableSep(lines[i + 1] || '')) {
      const parsed = parseTable(lines, i);
      blocks.push(parsed.block);
      i = parsed.next;
      continue;
    }
    const legacy = LEGACY_HEADING_RE.exec(line);
    if (legacy) {
      blocks.push(legacy[2].trim()
        ? { type: 'paragraph', inlines: [
            { t: 'strong', v: [{ t: 'text', v: `【${legacy[1]}】` }] },
            { t: 'text', v: ' ' },
            ...parseInline(legacy[2].trim()),
          ] }
        : { type: 'heading', level: 3, inlines: parseInline(legacy[1]) });
      i += 1;
      continue;
    }
    const para = [];
    while (i < lines.length) {
      const current = lines[i];
      if (!current.trim()) break;
      if (para.length && startsBlock(current, lines[i + 1])) break;
      para.push(current.trim());
      i += 1;
    }
    blocks.push({ type: 'paragraph', inlines: parseInline(para.join('\n')) });
  }
  return blocks;
}

function parseFence(lines, start) {
  const match = FENCE_RE.exec(lines[start]);
  const marker = match[1][0];
  const length = match[1].length;
  const info = (match[2] || '').trim();
  const body = [];
  const pad = indentOf(lines[start]);
  let i = start + 1;
  const closing = new RegExp('^[ \\t]*' + (marker === '`' ? '`' : '~') + '{' + length + ',}[ \\t]*$');
  while (i < lines.length && !closing.test(lines[i])) {
    let line = lines[i];
    for (let n = 0; n < pad && line.startsWith(' '); n += 1) line = line.slice(1);
    body.push(line);
    i += 1;
  }
  const next = i < lines.length ? i + 1 : i;
  const code = body.join('\n');
  if (RICH_FENCE.test(info)) {
    const spec = parseComponentJson(code);
    if (spec !== undefined) return { block: { type: 'components', spec }, next };
    return {
      block: { type: 'code', lang: 'json', code, notice: '组件块不是合法 JSON，先按代码显示' },
      next,
    };
  }
  return { block: { type: 'code', lang: info, code }, next };
}

function parseComponentJson(source) {
  const text = String(source || '').trim();
  if (!text) return [];
  try {
    return JSON.parse(text);
  } catch { /* 继续尝试宽松解析 */ }
  const repaired = text
    .replace(/^[^{[]*/, '')
    .replace(/[^}\]]*$/, '')
    .replace(/,\s*([}\]])/g, '$1')
    .replace(/[\u201c\u201d]/g, '"')
    .replace(/[\u2018\u2019]/g, "'");
  try {
    return JSON.parse(repaired);
  } catch {
    return undefined;
  }
}

function parseList(lines, start, depth) {
  const first = ITEM_RE.exec(lines[start]);
  const baseIndent = indentOf(first[1]);
  const ordered = /\d/.test(first[2]);
  const startNumber = ordered ? Number.parseInt(first[2], 10) || 1 : 1;
  const items = [];
  let i = start;
  while (i < lines.length) {
    const line = lines[i];
    if (!line.trim()) {
      let next = i + 1;
      while (next < lines.length && !lines[next].trim()) next += 1;
      if (next >= lines.length) break;
      const item = ITEM_RE.exec(lines[next]);
      const indent = indentOf(lines[next]);
      if ((item && indent >= baseIndent) || (!item && indent > baseIndent)) {
        i = next;
        continue;
      }
      break;
    }
    const item = ITEM_RE.exec(line);
    const indent = indentOf(line);
    if (!item) {
      if (indent > baseIndent && items.length) {
        // 列表项里嵌套的代码块、标题仍然按块处理，否则 ``` 会原样漏出来。
        if (FENCE_RE.test(line)) {
          const parsed = parseFence(lines, i);
          items[items.length - 1].blocks.push(parsed.block);
          i = parsed.next;
          continue;
        }
        const heading = HEADING_RE.exec(line);
        if (heading) {
          items[items.length - 1].blocks.push({ type: 'heading', level: heading[1].length, inlines: parseInline(heading[2].trim()) });
          i += 1;
          continue;
        }
        const para = [];
        while (i < lines.length && lines[i].trim() && indentOf(lines[i]) > baseIndent && !ITEM_RE.test(lines[i])) {
          para.push(lines[i].trim());
          i += 1;
        }
        items[items.length - 1].blocks.push({ type: 'paragraph', inlines: parseInline(para.join('\n')) });
        continue;
      }
      break;
    }
    if (indent < baseIndent) break;
    if (indent > baseIndent && depth < MAX_DEPTH) {
      const nested = parseList(lines, i, depth + 1);
      if (items.length) items[items.length - 1].blocks.push(nested.block);
      else items.push({ checked: null, inlines: [], blocks: [nested.block] });
      i = nested.next;
      continue;
    }
    const task = TASK_RE.exec(item[3]);
    items.push({
      checked: task ? task[1].toLowerCase() === 'x' : null,
      inlines: parseInline(task ? task[2] : item[3]),
      blocks: [],
    });
    i += 1;
  }
  return { block: { type: 'list', ordered, start: startNumber, items }, next: i };
}

function parseQuote(lines, start, depth) {
  const body = [];
  let i = start;
  while (i < lines.length) {
    const match = QUOTE_RE.exec(lines[i]);
    if (!match) break;
    body.push(match[1]);
    i += 1;
  }
  const joined = body.join('\n').trim();
  const alert = ALERT_RE.exec(body[0] || '');
  if (alert && depth < MAX_DEPTH) {
    const rest = [alert[2], ...body.slice(1)].join('\n').trim();
    return {
      block: {
        type: 'components',
        spec: { type: 'callout', tone: alert[1], content: rest },
      },
      next: i,
    };
  }
  return {
    block: { type: 'quote', blocks: depth < MAX_DEPTH ? parseBlocks(body, depth + 1) : [{ type: 'paragraph', inlines: parseInline(joined) }] },
    next: i,
  };
}

function parseTable(lines, start) {
  const split = (line) => line.trim().replace(/^\|/, '').replace(/\|$/, '')
    .split('|').map((cell) => cell.trim());
  const header = split(lines[start]);
  const align = split(lines[start + 1]).map((cell) =>
    cell.startsWith(':') && cell.endsWith(':') ? 'center'
      : cell.endsWith(':') ? 'right' : cell.startsWith(':') ? 'left' : '');
  const rows = [];
  let i = start + 2;
  while (i < lines.length && lines[i].trim() && lines[i].includes('|')) {
    rows.push(split(lines[i]));
    i += 1;
  }
  return {
    block: { type: 'table', header: header.map((cell) => parseInline(cell)), align, rows: rows.map((row) => row.map((cell) => parseInline(cell))) },
    next: i,
  };
}

/* ── 块渲染 ───────────────────────────────────────────────────────────── */

export function renderRichReply(host, source) {
  const blocks = parseRichReply(source);
  host.replaceChildren(...blocks.map((block) => renderBlock(block, 0)));
  return host;
}

function renderBlock(block, depth) {
  switch (block.type) {
    case 'heading': {
      const level = Math.min(6, Math.max(1, block.level));
      const node = el('h' + level, `ai-rich-h ai-rich-h${level}`);
      node.append(renderInline(block.inlines));
      return node;
    }
    case 'paragraph': {
      const node = el('p', 'ai-rich-p');
      node.append(renderInline(block.inlines));
      return node;
    }
    case 'list': {
      const node = document.createElement(block.ordered ? 'ol' : 'ul');
      node.className = 'ai-rich-list';
      if (block.ordered && block.start !== 1) node.start = block.start;
      for (const item of block.items) {
        const li = document.createElement('li');
        if (item.checked !== null) {
          li.className = 'ai-rich-task';
          const box = el('input');
          box.type = 'checkbox';
          box.disabled = true;
          box.checked = Boolean(item.checked);
          li.append(box);
        }
        if (item.inlines.length) li.append(renderInline(item.inlines));
        for (const child of item.blocks) li.append(renderBlock(child, depth));
        node.append(li);
      }
      return node;
    }
    case 'quote': {
      const node = el('blockquote', 'ai-rich-quote');
      for (const child of block.blocks) node.append(renderBlock(child, depth));
      return node;
    }
    case 'table': {
      const wrap = el('div', 'ai-rich-table-wrap');
      const table = el('table', 'ai-rich-table');
      const thead = el('thead');
      const headRow = el('tr');
      block.header.forEach((cell) => {
        const th = el('th');
        th.append(renderInline(cell));
        headRow.append(th);
      });
      thead.append(headRow);
      const tbody = el('tbody');
      for (const row of block.rows) {
        const tr = el('tr');
        row.forEach((cell) => {
          const td = el('td');
          td.append(renderInline(cell));
          tr.append(td);
        });
        tbody.append(tr);
      }
      table.append(thead, tbody);
      wrap.append(table);
      return wrap;
    }
    case 'hr':
      return el('hr', 'ai-rich-hr');
    case 'code': {
      const wrap = el('div', 'ai-rich-code');
      const head = el('div', 'ai-rich-code-head');
      head.append(el('span', 'ai-rich-code-lang', block.lang || '文本'));
      if (block.notice) head.append(el('span', 'ai-rich-code-notice', block.notice));
      head.append(copyButton(block.code, head));
      const pre = el('pre');
      const code = el('code');
      if (block.lang) code.dataset.lang = block.lang;
      code.textContent = block.code || '';
      pre.append(code);
      wrap.append(head, pre);
      return wrap;
    }
    case 'components':
      return renderComponents(block.spec, depth);
    default:
      return el('p', 'ai-rich-p', '');
  }
}

function copyButton(text, head) {
  const button = el('button', 'ai-rich-copy', '复制');
  button.type = 'button';
  button.addEventListener('click', async () => {
    const ok = await copyText(text);
    button.textContent = ok ? '已复制' : '复制失败';
    if (head) head.classList.toggle('ai-rich-copied', ok);
    setTimeout(() => {
      button.textContent = '复制';
      if (head) head.classList.remove('ai-rich-copied');
    }, 1400);
  });
  return button;
}

async function copyText(text) {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch { /* 退回 execCommand */ }
  try {
    const area = el('textarea');
    area.value = text;
    area.setAttribute('readonly', '');
    area.style.position = 'fixed';
    area.style.opacity = '0';
    document.body.append(area);
    area.select();
    const ok = document.execCommand('copy');
    area.remove();
    return ok;
  } catch {
    return false;
  }
}

/* ── 结构化 UI 组件 ───────────────────────────────────────────────────── */

function normalizeTone(tone, fallback = 'info') {
  if (!tone) return fallback;
  const name = String(tone).toLowerCase();
  return CALLOUT_TONES.has(name) ? name : TONE_ALIASES[name] || fallback;
}

function componentSpecs(spec) {
  if (Array.isArray(spec)) return spec;
  if (typeof spec === 'string' || typeof spec === 'number') return [{ type: 'markdown', content: String(spec) }];
  if (!spec || typeof spec !== 'object') return [];
  if (Array.isArray(spec.blocks)) return spec.blocks;
  return [spec];
}

function renderComponents(spec, depth) {
  const fragment = document.createDocumentFragment();
  for (const item of componentSpecs(spec)) {
    const node = renderComponent(item, depth);
    if (node) fragment.append(node);
  }
  if (!fragment.childNodes.length) fragment.append(el('p', 'ai-rich-p', ''));
  return fragment;
}

function renderComponent(spec, depth) {
  if (!spec || typeof spec !== 'object') {
    return el('p', 'ai-rich-p', typeof spec === 'string' ? spec : '');
  }
  const raw = spec.type || spec.kind
    || (typeof spec.text === 'string' || typeof spec.content === 'string' ? 'markdown' : 'json');
  const type = String(raw).toLowerCase().replace(/[_-]/g, '');
  if (depth > MAX_DEPTH) return el('p', 'ai-rich-p', '组件层级过深，已省略。');
  const alias = {
    kv: 'keyvalue', keyvaluepair: 'keyvalue', pairs: 'keyvalue', properties: 'keyvalue',
    list: 'list', bullets: 'list', items: 'list',
    steps: 'steps', step: 'steps', sequence: 'steps',
    timeline: 'timeline', events: 'timeline', log: 'timeline',
    stat: 'stat', metric: 'stat', kpi: 'stat',
    stats: 'stats', metrics: 'stats', statgrid: 'stats', numbers: 'stats',
    progress: 'progress', bar: 'progress', percent: 'progress',
    code: 'code', pre: 'code', snippet: 'code',
    diff: 'diff', patch: 'diff',
    table: 'table', gridtable: 'table',
    callout: 'callout', note: 'callout', alert: 'callout', tip: 'callout', warning: 'callout',
    card: 'card', section: 'card', panel: 'card', box: 'card',
    grid: 'grid', columns: 'grid', cols: 'grid',
    badge: 'badge', tag: 'badge', chip: 'badge',
    divider: 'divider', rule: 'divider', hr: 'divider',
    text: 'markdown', markdown: 'markdown', md: 'markdown', paragraph: 'markdown',
    json: 'json', raw: 'json',
  }[type] || type;

  switch (alias) {
    case 'callout': {
      const node = el('div', 'ai-rich-callout');
      node.dataset.tone = normalizeTone(spec.tone || spec.level || spec.variant);
      const body = el('div', 'ai-rich-callout-body');
      if (spec.title || spec.label) body.append(el('strong', 'ai-rich-callout-title', contentText(spec.title || spec.label)));
      body.append(renderMarkdownContent(spec.content ?? spec.text ?? spec.message, depth));
      node.append(body);
      return node;
    }
    case 'list': {
      const node = el('div', 'ai-rich-list-block');
      if (spec.title) node.append(el('div', 'ai-rich-block-title', contentText(spec.title)));
      const list = document.createElement(spec.ordered ? 'ol' : 'ul');
      list.className = 'ai-rich-list';
      for (const entry of arrayOf(spec.items ?? spec.value ?? spec.content)) {
        const li = document.createElement('li');
        if (entry && typeof entry === 'object') {
          const title = contentText(entry.title ?? entry.label ?? entry.name ?? entry.text ?? '');
          const detail = entry.description ?? entry.detail ?? entry.note ?? entry.value ?? '';
          const strong = el('strong', 'ai-rich-item-title', title);
          li.append(strong);
          if (Array.isArray(entry.items)) {
            const inner = document.createElement('ul');
            inner.className = 'ai-rich-list';
            for (const sub of entry.items) inner.append(el('li', '', contentText(sub)));
            li.append(inner);
          } else if (detail !== '' && detail !== undefined && detail !== null) {
            const detailNode = el('span', 'ai-rich-item-detail');
            detailNode.append(inlineNodes(detail));
            li.append(detailNode);
          }
        } else {
          li.append(inlineNodes(entry));
        }
        list.append(li);
      }
      node.append(list);
      return node;
    }
    case 'steps': {
      const node = el('div', 'ai-rich-block');
      if (spec.title) node.append(el('div', 'ai-rich-block-title', contentText(spec.title)));
      const list = el('ol', 'ai-rich-steps');
      for (const entry of arrayOf(spec.steps ?? spec.items ?? spec.content)) {
        const li = document.createElement('li');
        if (entry && typeof entry === 'object') {
          li.append(el('strong', 'ai-rich-step-title', contentText(entry.title ?? entry.label ?? entry.name ?? '')));
          const detail = entry.description ?? entry.detail ?? entry.note ?? entry.content ?? '';
          if (detail) {
            const detailNode = el('span', 'ai-rich-step-detail');
            detailNode.append(inlineNodes(detail));
            li.append(detailNode);
          }
        } else {
          li.append(inlineNodes(entry));
        }
        list.append(li);
      }
      node.append(list);
      return node;
    }
    case 'timeline': {
      const node = el('div', 'ai-rich-block');
      if (spec.title) node.append(el('div', 'ai-rich-block-title', contentText(spec.title)));
      const list = el('ul', 'ai-rich-timeline');
      for (const entry of arrayOf(spec.items ?? spec.events ?? spec.content)) {
        const li = document.createElement('li');
        if (entry && typeof entry === 'object') {
          const time = entry.time ?? entry.at ?? entry.timestamp ?? '';
          const head = el('div', 'ai-rich-event-head');
          if (time) head.append(el('time', 'ai-rich-time', contentText(time)));
          head.append(el('strong', 'ai-rich-event-title', contentText(entry.title ?? entry.label ?? entry.name ?? '')));
          li.append(head);
          const detail = entry.description ?? entry.detail ?? entry.note ?? entry.text ?? '';
          if (detail) {
            const detailNode = el('span', 'ai-rich-event-detail');
            detailNode.append(inlineNodes(detail));
            li.append(detailNode);
          }
        } else {
          li.append(inlineNodes(entry));
        }
        list.append(li);
      }
      node.append(list);
      return node;
    }
    case 'stat':
    case 'stats': {
      const node = el('div', 'ai-rich-stats');
      const entries = alias === 'stat'
        ? [spec]
        : arrayOf(spec.items ?? spec.stats ?? spec.metrics ?? spec.content);
      for (const entry of entries) {
        const cell = el('div', 'ai-rich-stat');
        const label = contentText(entry?.label ?? entry?.title ?? entry?.name ?? '');
        const value = contentText(entry?.value ?? entry?.amount ?? entry?.text ?? '');
        if (label) cell.append(el('span', 'ai-rich-stat-label', label));
        cell.append(el('strong', 'ai-rich-stat-value', value));
        const detail = entry?.detail ?? entry?.description ?? entry?.note ?? '';
        if (detail) cell.append(el('span', 'ai-rich-stat-detail', contentText(detail)));
        node.append(cell);
      }
      return node;
    }
    case 'progress': {
      const node = el('div', 'ai-rich-progress');
      const ratio = progressRatio(spec.value ?? spec.percent ?? spec.progress ?? 0);
      const head = el('div', 'ai-rich-progress-head');
      head.append(el('span', 'ai-rich-progress-label', contentText(spec.label ?? spec.title ?? '')));
      head.append(el('strong', 'ai-rich-progress-value', spec.display ? contentText(spec.display) : `${Math.round(ratio * 100)}%`));
      const track = el('div', 'ai-rich-progress-track');
      const fill = el('span', 'ai-rich-progress-fill');
      fill.style.width = `${Math.round(ratio * 1000) / 10}%`;
      track.append(fill);
      node.append(head, track);
      const detail = spec.detail ?? spec.description ?? spec.note ?? '';
      if (detail) node.append(renderMarkdownContent(detail, depth + 1, 'ai-rich-progress-detail'));
      return node;
    }
    case 'code': {
      const wrap = el('div', 'ai-rich-code');
      const head = el('div', 'ai-rich-code-head');
      head.append(el('span', 'ai-rich-code-lang', contentText(spec.title || spec.language || spec.lang || '代码')));
      const text = codeText(spec);
      head.append(copyButton(text, head));
      const pre = el('pre');
      const code = el('code');
      if (spec.language || spec.lang) code.dataset.lang = contentText(spec.language || spec.lang);
      code.textContent = text;
      pre.append(code);
      wrap.append(head, pre);
      return wrap;
    }
    case 'diff': {
      const node = el('div', 'ai-rich-diff');
      const lines = arrayOf(spec.lines ?? spec.items ?? spec.content ?? spec.text);
      for (const entry of lines) {
        const line = typeof entry === 'string' ? diffLineFromText(entry) : entry;
        if (!line) continue;
        const kind = String(line.type ?? line.kind ?? 'context').toLowerCase();
        const row = el('div', 'ai-rich-diff-line');
        row.dataset.kind = /^(add|added|\+|insert)/.test(kind) ? 'add'
          : /^(remove|removed|-|delete|del)/.test(kind) ? 'remove' : 'context';
        row.append(el('span', 'ai-rich-diff-sign', row.dataset.kind === 'add' ? '+' : row.dataset.kind === 'remove' ? '−' : ' '));
        row.append(el('span', 'ai-rich-diff-text', contentText(line.text ?? line.content ?? line.value ?? '')));
        node.append(row);
      }
      if (!node.childNodes.length) node.append(el('div', 'ai-rich-diff-line', '没有改动'));
      return node;
    }
    case 'table': {
      const wrap = el('div', 'ai-rich-table-wrap');
      const table = el('table', 'ai-rich-table');
      const columns = arrayOf(spec.columns ?? spec.header ?? spec.headers);
      const rows = arrayOf(spec.rows ?? spec.items ?? spec.data);
      if (columns.length) {
        const thead = el('thead');
        const tr = el('tr');
        columns.forEach((column) => {
          const th = el('th');
          th.append(inlineNodes(column && typeof column === 'object' ? column.label ?? column.title ?? column : column));
          tr.append(th);
        });
        thead.append(tr);
        table.append(thead);
      }
      const tbody = el('tbody');
      for (const row of rows) {
        const tr = el('tr');
        for (const cell of arrayOf(row)) {
          const td = el('td');
          td.append(inlineNodes(cell));
          tr.append(td);
        }
        tbody.append(tr);
      }
      table.append(tbody);
      wrap.append(table);
      return wrap;
    }
    case 'keyvalue': {
      const node = el('div', 'ai-rich-block');
      if (spec.title) node.append(el('div', 'ai-rich-block-title', contentText(spec.title)));
      const list = el('dl', 'ai-rich-kv');
      const pairs = spec.pairs ?? spec.items ?? spec.rows ?? spec.fields;
      if (Array.isArray(pairs)) {
        for (const pair of pairs) {
          const row = el('div', 'ai-rich-kv-row');
          if (Array.isArray(pair)) {
            row.append(el('dt', '', contentText(pair[0])));
            const value = el('dd');
            value.append(inlineNodes(pair[1]));
            row.append(value);
          } else if (pair && typeof pair === 'object') {
            row.append(el('dt', '', contentText(pair.key ?? pair.label ?? pair.name ?? '')));
            const value = el('dd');
            value.append(inlineNodes(pair.value ?? pair.text ?? ''));
            row.append(value);
          }
          list.append(row);
        }
      } else if (pairs && typeof pairs === 'object') {
        for (const [key, value] of Object.entries(pairs)) {
          const row = el('div', 'ai-rich-kv-row');
          row.append(el('dt', '', key));
          const cellValue = el('dd');
          cellValue.append(inlineNodes(value));
          row.append(cellValue);
          list.append(row);
        }
      }
      node.append(list);
      return node;
    }
    case 'card': {
      const node = el('section', 'ai-rich-card');
      const title = spec.title ?? spec.label ?? '';
      if (title) {
        const header = el('header', 'ai-rich-card-head');
        header.append(el('h4', '', contentText(title)));
        if (spec.badge) header.append(renderBadge(spec.badge));
        node.append(header);
      }
      const body = el('div', 'ai-rich-card-body');
      if (spec.content !== undefined || spec.text !== undefined) {
        body.append(renderMarkdownContent(spec.content ?? spec.text, depth + 1));
      }
      for (const child of arrayOf(spec.children ?? spec.blocks ?? spec.items)) {
        if (child && typeof child === 'object' && (child.type || child.kind)) body.append(renderComponent(child, depth + 1));
        else if (child !== undefined && child !== null) body.append(renderMarkdownContent(contentText(child), depth + 1));
      }
      node.append(body);
      return node;
    }
    case 'grid': {
      const node = el('div', 'ai-rich-grid');
      const columns = Number(spec.columns ?? spec.cols ?? spec.count ?? 2);
      if (Number.isFinite(columns) && columns > 0) node.style.setProperty('--ai-grid-cols', String(Math.min(4, Math.round(columns))));
      for (const child of arrayOf(spec.children ?? spec.items ?? spec.blocks)) {
        if (child && typeof child === 'object' && (child.type || child.kind)) node.append(renderComponent(child, depth + 1));
        else node.append(renderMarkdownContent(contentText(child), depth + 1));
      }
      return node;
    }
    case 'badge':
      return renderBadge(spec);
    case 'divider': {
      const node = el('div', 'ai-rich-divider');
      if (spec.label) node.append(el('span', '', contentText(spec.label)));
      return node;
    }
    case 'markdown':
      return renderMarkdownContent(spec.content ?? spec.text ?? spec.value ?? '', depth);
    case 'json': {
      const wrap = el('div', 'ai-rich-code');
      const head = el('div', 'ai-rich-code-head');
      head.append(el('span', 'ai-rich-code-lang', contentText(spec.title || 'JSON')));
      const text = typeof spec.value === 'string' ? spec.value
        : JSON.stringify(spec.value ?? spec.content ?? spec, null, 2);
      head.append(copyButton(text, head));
      const pre = el('pre');
      const code = el('code');
      code.textContent = text;
      pre.append(code);
      wrap.append(head, pre);
      return wrap;
    }
    default: {
      const node = el('div', 'ai-rich-unknown');
      node.append(el('span', 'ai-rich-unknown-title', `未知组件：${type}`));
      const pre = el('pre');
      pre.textContent = contentText(spec);
      node.append(pre);
      return node;
    }
  }
}

function renderBadge(badge) {
  const node = el('span', 'ai-rich-badge');
  if (badge && typeof badge === 'object') {
    node.dataset.tone = normalizeTone(badge.tone, 'info');
    node.textContent = contentText(badge.text ?? badge.label ?? badge.value ?? '');
  } else {
    node.dataset.tone = 'info';
    node.textContent = contentText(badge);
  }
  return node;
}

function renderMarkdownContent(content, depth, className = '') {
  const node = el('div', className ? `ai-rich-body ${className}` : 'ai-rich-body');
  if (content === undefined || content === null || content === '') return node;
  if (typeof content !== 'string' && typeof content !== 'number' && typeof content !== 'boolean') {
    node.append(renderComponent(content, depth + 1));
    return node;
  }
  if (depth > MAX_DEPTH) {
    node.textContent = String(content);
    return node;
  }
  const blocks = parseRichReply(String(content), depth + 1);
  for (const block of blocks) node.append(renderBlock(block, depth + 1));
  return node;
}

function contentText(value) {
  if (value === undefined || value === null) return '';
  if (typeof value === 'string') return value;
  if (typeof value === 'number' || typeof value === 'boolean') return String(value);
  if (Array.isArray(value)) return value.map(contentText).join(' · ');
  if (typeof value === 'object') {
    const picked = value.text ?? value.label ?? value.title ?? value.value;
    if (picked !== undefined && picked !== null && typeof picked !== 'object') return String(picked);
    try {
      return JSON.stringify(value);
    } catch {
      return String(value);
    }
  }
  return String(value);
}

function arrayOf(value) {
  if (value === undefined || value === null) return [];
  return Array.isArray(value) ? value : [value];
}

function codeText(spec) {
  const value = spec.content ?? spec.code ?? spec.text ?? spec.value ?? '';
  if (typeof value === 'string') return value;
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

function progressRatio(value) {
  if (typeof value === 'string') {
    const parsed = Number.parseFloat(value.replace('%', '').trim());
    if (!Number.isFinite(parsed)) return 0;
    return Math.min(1, Math.max(0, value.includes('%') ? parsed / 100 : parsed > 1 ? parsed / 100 : parsed));
  }
  const number = Number(value);
  if (!Number.isFinite(number)) return 0;
  return Math.min(1, Math.max(0, number > 1 ? number / 100 : number));
}

function diffLineFromText(line) {
  const text = String(line);
  if (/^\+(?!\+\+)/.test(text)) return { type: 'add', text: text.slice(1).trimStart() };
  if (/^-(?!--)/.test(text)) return { type: 'remove', text: text.slice(1).trimStart() };
  return { type: 'context', text: text.replace(/^ /, '') };
}
