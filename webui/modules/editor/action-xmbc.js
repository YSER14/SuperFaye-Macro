// -*- coding: utf-8 -*-
/**
 * 「可视化动作列表」的第二种显示方式：罗技 / 拼宏式横向方块流。
 *
 * 只影响显示层：宏文件内容、拖拽排序、loop 展开、批量注释、执行逻辑全都不变；
 * 积木块视图（中文动作名，一行一条）仍是默认视图。
 *
 * 方块口径（对齐 X-Mouse Button Control 的 {TAG} 与宏格式参考）：
 *     键动作      -> 一块「e / ↓」：↓ = 按下、↑ = 松开；动作后的延迟单独一块虚线「300ms」
 *     鼠标键动作  -> 左/右键换成罗技鼠标标识（按下的那半边填色）+ ↓/↑；中键、侧键仍用文字名
 *     wait        -> 一块「300ms」：不再写“延迟”，只留毫秒数
 *     view        -> 一块「视角 / 0,2 · 8.441ms」
 *     loop        -> 「循环 / ×3」…「结束循环」
 *     import      -> 「调用 / 子宏名」
 *     note        -> 「注释 / 文本」
 *
 * 代码面板用同一套标签词汇，方便对照：{LMBD}{WAITMS:300}{LMBU}…
 * 其中 {MOVE}/{REPEAT}/{IMPORT}/{NOTE} 是本项目扩展，XMBC 原生没有。
 */

import { formatActionLabel, getRecoverableActionFromNoteText, parseNumber, parseLoopCount } from './action-model.js';
import { RECOVERABLE_NOTE_PREFIX } from '../core/constants.js';

export const ACTION_VIEW_BLOCK = 'block';
export const ACTION_VIEW_TIMELINE = 'timeline';
export const ACTION_VIEW_STORAGE_KEY = 'macrodriver-action-view';
// 早期版本存过 'xmbc'，读到就迁移到时间轴
const LEGACY_VIEW_XMBC = 'xmbc';

const MOUSE_NAMES = {
  left: '左键', right: '右键', middle: '中键',
  x1: '侧键1', x2: '侧键2', '4': '侧键1', '5': '侧键2',
};

const MOUSE_TAGS = {
  left: 'LMB', right: 'RMB', middle: 'MMB',
  x1: 'MB4', x2: 'MB5', '4': 'MB4', '5': 'MB5',
};

const KEY_TAGS = {
  space: '{SPACE}', enter: '{ENTER}', tab: '{TAB}', esc: '{ESC}', escape: '{ESC}',
  back: '{BACK}', backspace: '{BACK}', delete: '{DEL}', del: '{DEL}',
  insert: '{INS}', ins: '{INS}', home: '{HOME}', end: '{END}',
  page_up: '{PGUP}', pgup: '{PGUP}', page_down: '{PGDN}', pgdn: '{PGDN}',
  up_arrow: '{UP}', down_arrow: '{DOWN}', left_arrow: '{LEFT}', right_arrow: '{RIGHT}',
  up: '{UP}', down: '{DOWN}', left: '{LEFT}', right: '{RIGHT}',
  ctrl: '{CTRL}', control: '{CTRL}', shift: '{SHIFT}', alt: '{ALT}', win: '{WIN}', meta: '{WIN}',
  caps_lock: '{CAPSLOCKON}', num_lock: '{NUMLOCKON}', scroll_lock: '{SCROLLLOCKON}',
};

function fmtMs(value) {
  const n = Number(value);
  if (!Number.isFinite(n)) return '0';
  return String(Math.round(n * 1000) / 1000);
}

function fmtSeconds(ms) {
  const n = Number(ms) || 0;
  return `${(n / 1000).toFixed(2)} s`;
}

function delayToken(ms) {
  const n = Number(ms);
  return Number.isFinite(n) && n > 0 ? `{WAITMS:${fmtMs(n)}}` : '';
}

/** 单个按键名 -> XMBC 标签（字母/数字原样输出，XMBC 里裸字符就是按一下） */
export function xmbcKeyToken(key) {
  const raw = String(key ?? '').trim();
  if (!raw) return '{?}';
  const lower = raw.toLowerCase();

  if (KEY_TAGS[lower]) return KEY_TAGS[lower];
  if (/^[a-z0-9]$/.test(lower)) return lower;
  if (/^f([1-9]|1[0-2])$/.test(lower)) return `{${lower.toUpperCase()}}`;
  if (/^num[0-9]$/.test(lower)) return `{NUM${lower.slice(3)}}`;

  // 没有 XMBC 等价标签，显式标成本项目扩展，不假装能往返
  return `{KEY:${lower}}`;
}

/** 一条动作 -> XMBC 宏串片段（代码面板 / 复制用） */
export function formatActionXmbc(action, expanded = false) {
  if (!Array.isArray(action) || !action.length) return '[]';

  const cmd = String(action[0]).trim().toLowerCase();

  if (cmd === 'kd' || cmd === 'ku') {
    const key = xmbcKeyToken(action[1]);
    const prefix = cmd === 'kd' ? '{PRESS}' : '{RELEASE}';
    return `${prefix}${key}${delayToken(action[2])}`;
  }

  if (cmd === 'md' || cmd === 'mu') {
    const button = String(action[1] ?? 'left').trim().toLowerCase();
    const base = MOUSE_TAGS[button] || `KEY:${button}`;
    const tag = `{${base}${cmd === 'md' ? 'D' : 'U'}}`;
    return `${tag}${delayToken(action[2])}`;
  }

  if (cmd === 'wait') {
    return `{WAITMS:${fmtMs(parseNumber(action[1], 0))}}`;
  }

  if (cmd === 'view') {
    const vec = Array.isArray(action[1]) ? action[1] : [0, 0];
    return `{MOVE:${fmtMs(parseNumber(vec[0], 0))},${fmtMs(parseNumber(vec[1], 0))},${fmtMs(Math.max(1, parseNumber(action[2], 50)))}}`;
  }

  if (cmd === 'scroll') {
    const delta = parseNumber(action[1], 0);
    const times = Math.max(1, Math.round(Math.abs(delta) / 120));
    return `{${delta >= 0 ? 'MWUP' : 'MWDN'}}`.repeat(times);
  }

  if (cmd === 'loop') {
    const count = parseLoopCount(action[1]);
    return expanded ? `{REPEAT:${count}}` : `{REPEAT:${count}} … {/REPEAT}`;
  }

  if (cmd === 'group') {
    // 宏组不落盘：预览时直接倒出内容，没有任何 XMBC 标签
    return buildXmbcLine(Array.isArray(action[2]) ? action[2] : []);
  }

  if (cmd === 'import') {
    return `{IMPORT:"${String(action[1] ?? '').trim() || '（空路径）'}"}`;
  }

  if (cmd === 'note') {
    const text = String(action[1] ?? '').trim();
    const restored = getRecoverableActionFromNoteText(text, RECOVERABLE_NOTE_PREFIX);
    return restored ? `// ↺ ${formatActionXmbc(restored, false)}` : `// ${text}`;
  }

  return JSON.stringify(action);
}

/* ───────────────────────────── 方块流 ───────────────────────────── */

const SVG_NS = 'http://www.w3.org/2000/svg';
let mouseGlyphSeq = 0;

function svgEl(tag, attrs, className) {
  const el = document.createElementNS(SVG_NS, tag);
  Object.entries(attrs || {}).forEach(([key, value]) => el.setAttribute(key, String(value)));
  if (className) el.setAttribute('class', className);
  return el;
}

/**
 * 罗技鼠标宏的标识符号：一个鼠标外形，上半部被竖缝分成左右两键，
 * 按下的那个键整块填色（左键填左半、右键填右半）。
 * 中键 / 侧键没有对应图形，调用方继续用文字名，只把副标题换成 ↓ / ↑。
 */
export function createMouseGlyph(button) {
  const side = String(button).toLowerCase() === 'right' ? 'right' : 'left';
  const clipId = `tl-mouse-clip-${++mouseGlyphSeq}`;
  const svg = svgEl('svg', { viewBox: '0 0 26 34' }, 'tl-mouse');
  svg.setAttribute('aria-hidden', 'true');
  svg.setAttribute('focusable', 'false');

  const defs = svgEl('defs', {});
  const clip = svgEl('clipPath', { id: clipId });
  clip.appendChild(svgEl('rect', { x: 3.5, y: 3.5, width: 19, height: 27, rx: 9.5 }));
  defs.appendChild(clip);
  svg.appendChild(defs);

  svg.appendChild(svgEl('rect', { x: 3.5, y: 3.5, width: 19, height: 27, rx: 9.5 }, 'tl-mouse-body'));

  const highlight = svgEl('g', { 'clip-path': `url(#${clipId})` });
  highlight.appendChild(svgEl('rect', {
    x: side === 'left' ? 0 : 13,
    y: 0,
    width: 13,
    height: 16.5,
  }, 'tl-mouse-hl'));
  svg.appendChild(highlight);

  svg.appendChild(svgEl('line', { x1: 3.5, y1: 16.5, x2: 22.5, y2: 16.5 }, 'tl-mouse-seam'));
  svg.appendChild(svgEl('line', { x1: 13, y1: 3.5, x2: 13, y2: 16.5 }, 'tl-mouse-seam'));
  return svg;
}

/**
 * 一块方块：主标题 + 副标题（可省），tone 决定配色。
 * mouse = 'left' | 'right' 时主标题位置换成鼠标标识，副标题留给 ↓ / ↑。
 */
export function createChip(main, sub, tone, title, mouse) {
  const doc = document;
  const chip = doc.createElement('span');
  chip.className = `tl-chip${tone ? ` tl-${tone}` : ''}${mouse ? ' tl-mouse-chip' : ''}`;

  if (mouse) chip.appendChild(createMouseGlyph(mouse));

  if (main) {
    const mainEl = doc.createElement('span');
    mainEl.className = 'tl-main';
    mainEl.textContent = main;
    chip.appendChild(mainEl);
  }

  if (sub) {
    const subEl = doc.createElement('span');
    subEl.className = 'tl-sub';
    subEl.textContent = sub;
    chip.appendChild(subEl);
  }

  if (title) chip.title = title;
  return chip;
}

/** 方块之间的箭头 */
export function createArrow() {
  const arrow = document.createElement('span');
  arrow.className = 'tl-arrow';
  arrow.textContent = '→';
  return arrow;
}

/**
 * 一条动作 -> 一块方块要显示什么。
 * 动作后的延迟不在这里出，由时间轴视图单独插一块虚线延迟。
 */
export function describeActionChip(action, expanded = false) {
  if (!Array.isArray(action) || !action.length) {
    return { main: '无法识别', sub: '', tone: 'muted', title: JSON.stringify(action) };
  }

  const cmd = String(action[0]).trim().toLowerCase();
  const title = formatActionLabel(action, expanded);

  if (cmd === 'kd' || cmd === 'ku') {
    return { main: String(action[1] ?? ''), sub: cmd === 'kd' ? '↓' : '↑', tone: 'key', title };
  }

  if (cmd === 'md' || cmd === 'mu') {
    const button = String(action[1] ?? 'left').trim().toLowerCase();
    const state = cmd === 'md' ? '↓' : '↑';
    // 左键 / 右键用罗技鼠标标识，文字名让位给图形；中键、侧键保留文字
    if (button === 'left' || button === 'right') {
      return { main: '', sub: state, tone: 'key', mouse: button, title };
    }
    return { main: MOUSE_NAMES[button] || button, sub: state, tone: 'key', title };
  }

  if (cmd === 'wait') {
    // 延迟块只留毫秒数，“延迟”二字不再占一行
    return { main: `${fmtMs(parseNumber(action[1], 0))}ms`, sub: '', tone: 'delay-solid', title };
  }

  if (cmd === 'view') {
    const vec = Array.isArray(action[1]) ? action[1] : [0, 0];
    return {
      main: '视角',
      sub: `${fmtMs(parseNumber(vec[0], 0))},${fmtMs(parseNumber(vec[1], 0))} · ${fmtMs(Math.max(1, parseNumber(action[2], 50)))}ms`,
      tone: 'view',
      title,
    };
  }

  if (cmd === 'scroll') {
    const delta = parseNumber(action[1], 0);
    return { main: '滚轮', sub: delta >= 0 ? '上滚' : '下滚', tone: 'view', title };
  }

  if (cmd === 'loop') {
    const count = parseLoopCount(action[1]);
    return { main: '循环', sub: `×${count}`, tone: 'flow', title };
  }

  if (cmd === 'group') {
    const name = String(action[1] ?? '').trim() || '未命名';
    const block = Array.isArray(action[2]) ? action[2] : [];
    return { main: '宏组', sub: `${name} · ${block.length} 条`, tone: 'group', title };
  }

  if (cmd === 'import') {
    const path = String(action[1] ?? '').trim();
    const short = path.split('/').pop() || path || '（空路径）';
    return { main: '调用', sub: short, tone: 'flow', title: `调用子宏 ${path}` };
  }

  if (cmd === 'note') {
    const text = String(action[1] ?? '').trim();
    const restored = getRecoverableActionFromNoteText(text, RECOVERABLE_NOTE_PREFIX);
    if (restored) {
      return { main: '注释', sub: '可恢复', tone: 'muted', title: '这条注释里存着一个被注释掉的动作，点一下可恢复' };
    }
    const short = text.length > 16 ? `${text.slice(0, 16)}…` : text || '（空注释）';
    return { main: '注释', sub: short, tone: 'muted', title: text };
  }

  return { main: cmd || '未知', sub: '', tone: 'muted', title: JSON.stringify(action) };
}

/* ───────────────────────────── 生成的宏代码 ───────────────────────────── */

/** 整份宏压成一整行 XMBC 宏串 */
export function buildXmbcLine(rows) {
  const walk = (list) => {
    let out = '';
    (Array.isArray(list) ? list : []).forEach((row) => {
      if (!Array.isArray(row) || !row.length) return;
      const cmd = String(row[0]).toLowerCase();
      if (cmd === 'loop') {
        const count = parseLoopCount(row[1]);
        if (count > 0) out += `{REPEAT:${count}}${walk(Array.isArray(row[2]) ? row[2] : [])}{/REPEAT}`;
        return;
      }
      // 宏组只是编辑期的抽屉：预览的宏串里把内容按顺序倒出来，不留任何标记
      if (cmd === 'group') {
        out += walk(Array.isArray(row[2]) ? row[2] : []);
        return;
      }
      if (cmd === 'note') {
        const text = String(row[1] ?? '').trim().replace(/\\/g, '\\\\').replace(/"/g, '\\"');
        out += `{NOTE:"${text}"}`;
        return;
      }
      out += formatActionXmbc(row, false);
    });
    return out;
  };
  return walk(rows);
}

/** 方块数 / 总时长 / 代码字数 */
export function buildMacroStats(rows) {
  let blocks = 0;
  let durationMs = 0;

  const walk = (list) => {
    (Array.isArray(list) ? list : []).forEach((row) => {
      if (!Array.isArray(row) || !row.length) return;
      const cmd = String(row[0]).toLowerCase();

      if (cmd === 'loop') {
        blocks += 1;
        const count = parseLoopCount(row[1]);
        if (count === 0) return;
        const before = durationMs;
        walk(Array.isArray(row[2]) ? row[2] : []);
        durationMs = before + (durationMs - before) * count;
        return;
      }

      if (cmd === 'group') {
        blocks += 1;
        walk(Array.isArray(row[2]) ? row[2] : []);
        return;
      }

      blocks += 1;
      if (cmd === 'wait') {
        durationMs += Math.max(0, parseNumber(row[1], 0));
        return;
      }
      if (cmd === 'view') {
        durationMs += Math.max(1, parseNumber(row[2], 50));
        return;
      }
      if (cmd === 'kd' || cmd === 'ku' || cmd === 'md' || cmd === 'mu') {
        const delay = Math.max(0, parseNumber(row[2], 0));
        if (delay > 0) blocks += 1;
        durationMs += delay;
      }
    });
  };

  walk(rows);
  return { blocks, durationMs, chars: buildXmbcLine(rows).length };
}

/** 代码面板：只在时间轴视图显示 */
export function updateTimelineCodeBar(rows, viewMode) {
  const bar = document.getElementById('timelineCodeBar');
  const code = document.getElementById('timelineCode');
  const stats = document.getElementById('timelineStats');
  if (!bar || !code) return;

  const isTimeline = viewMode === ACTION_VIEW_TIMELINE;
  bar.classList.toggle('hidden', !isTimeline);
  if (!isTimeline) {
    code.textContent = '';
    if (stats) stats.textContent = '';
    return;
  }

  const info = buildMacroStats(rows);
  code.textContent = buildXmbcLine(rows) || '（空宏）';
  if (stats) {
    stats.textContent = `${info.blocks} 块 · ${fmtSeconds(info.durationMs)} · ${info.chars} 字`;
  }
}

/** 视图切换按钮高亮 */
export function refreshViewModeButtons(viewMode) {
  document.querySelectorAll('#actionViewSwitch [data-view-mode]').forEach((btn) => {
    btn.classList.toggle('active', btn.dataset.viewMode === viewMode);
  });
}

/** 读存档的显示方式；老版本存的 'xmbc' 迁移到时间轴 */
export function readStoredViewMode(storage) {
  try {
    const saved = storage?.getItem(ACTION_VIEW_STORAGE_KEY);
    if (saved === ACTION_VIEW_TIMELINE || saved === LEGACY_VIEW_XMBC) return ACTION_VIEW_TIMELINE;
  } catch (err) {
    /* 隐私模式拿不到 localStorage，退回默认视图 */
  }
  return ACTION_VIEW_BLOCK;
}
