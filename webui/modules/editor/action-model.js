import { RECOVERABLE_NOTE_PREFIX } from '../core/constants.js';

export function parseNumber(text, fallback = 0) {
  const n = Number(text);
  return Number.isFinite(n) ? n : fallback;
}

export function pathKey(path) {
  return Array.isArray(path) ? path.join('.') : '';
}

export function keyToPath(key) {
  if (!key) return null;
  return String(key)
    .split('.')
    .filter((part) => part.length > 0)
    .map((part) => Number.parseInt(part, 10))
    .filter((num) => Number.isInteger(num) && num >= 0);
}

export function cloneAction(action) {
  return JSON.parse(JSON.stringify(action));
}

export function isLoopAction(action) {
  return Array.isArray(action)
    && action.length >= 3
    && String(action[0]).trim().toLowerCase() === 'loop'
    && Array.isArray(action[2]);
}

/**
 * 宏组：`['group', '组名', [子动作…]]`。
 * 纯组织容器 —— 和循环一样能在编辑器里展开/嵌套/拖拽，但保存前会被摊平
 * （宏执行引擎不认识 group 指令，所以它绝不会写进宏文件）。
 */
export function isGroupAction(action) {
  return Array.isArray(action)
    && action.length >= 3
    && String(action[0]).trim().toLowerCase() === 'group'
    && Array.isArray(action[2]);
}

/** 能装子动作的容器：循环、宏组 */
export function isContainerAction(action) {
  return isLoopAction(action) || isGroupAction(action);
}

/** 容器种类：'loop' / 'group' / null */
export function containerKind(action) {
  if (isLoopAction(action)) return 'loop';
  if (isGroupAction(action)) return 'group';
  return null;
}

/** 取容器里的子动作数组；不是容器返回 null */
export function getContainerBody(action) {
  return isContainerAction(action) ? action[2] : null;
}

/**
 * 摊平宏组：宏组只是编排用的抽屉，存盘 / 生成宏串前把它的内容按顺序倒出来。
 * 循环原样保留（`{REPEAT}` 是引擎真的支持的指令）。
 */
export function flattenGroups(rows) {
  const walk = (list) => {
    const out = [];
    (Array.isArray(list) ? list : []).forEach((row) => {
      if (isGroupAction(row)) {
        out.push(...walk(row[2]));
        return;
      }
      if (isLoopAction(row)) {
        out.push(['loop', row[1], walk(row[2])]);
        return;
      }
      out.push(cloneAction(row));
    });
    return out;
  };
  return walk(rows);
}

/** 递归数动作条数（宏组/循环都算进去），给块库计数用 */
export function countActionsDeep(rows) {
  let n = 0;
  (Array.isArray(rows) ? rows : []).forEach((row) => {
    n += 1;
    if (isContainerAction(row)) n += countActionsDeep(row[2]);
  });
  return n;
}

/** 列出 rows 里所有宏组路径（含嵌套），路径用 `父/子` 表示 */
export function findGroupPaths(rows, prefix = '') {
  const out = [];
  (Array.isArray(rows) ? rows : []).forEach((row) => {
    if (!isContainerAction(row)) return;
    if (isGroupAction(row)) {
      const name = String(row[1] ?? '').trim() || '未命名';
      const path = prefix ? `${prefix}/${name}` : name;
      out.push(path);
      out.push(...findGroupPaths(row[2], path));
      return;
    }
    out.push(...findGroupPaths(row[2], prefix));
  });
  return out;
}

/** 按 `父/子` 路径取某个宏组的子动作；取不到返回 [] */
export function selectGroupBody(rows, groupPath) {
  let cur = Array.isArray(rows) ? rows : [];
  if (!groupPath) return cur;
  String(groupPath).split('/').filter(Boolean).forEach((part) => {
    const hit = cur.find((row) => isGroupAction(row) && (String(row[1] ?? '').trim() || '未命名') === part);
    cur = hit ? hit[2] : [];
  });
  return cur;
}

export function normalizeAction(action) {
  if (!Array.isArray(action) || !action.length) {
    return ['wait', 50];
  }

  const cmd = String(action[0]).trim().toLowerCase();

  if (cmd === 'loop') {
    const count = Math.max(1, parseNumber(action[1], 1));
    const block = Array.isArray(action[2]) ? action[2].map((row) => normalizeAction(row)) : [];
    return ['loop', count, block];
  }

  if (cmd === 'group') {
    const name = String(action[1] ?? '').trim() || '宏组';
    const block = Array.isArray(action[2]) ? action[2].map((row) => normalizeAction(row)) : [];
    return ['group', name, block];
  }

  if (cmd === 'view') {
    const vec = Array.isArray(action[1]) ? action[1] : [0, 0];
    return ['view', [parseNumber(vec[0], 0), parseNumber(vec[1], 0)], Math.max(1, parseNumber(action[2], 50))];
  }

  if (cmd === 'wait') {
    return ['wait', Math.max(0, parseNumber(action[1], 50))];
  }

  if (cmd === 'import') {
    return ['import', String(action[1] ?? '').trim()];
  }

  if (cmd === 'note') {
    return ['note', String(action[1] ?? '').trim()];
  }

  if (['kd', 'ku', 'md', 'mu'].includes(cmd)) {
    const fallbackKey = cmd === 'md' || cmd === 'mu' ? 'left' : 'w';
    return [cmd, String(action[1] ?? fallbackKey), Math.max(0, parseNumber(action[2], 50))];
  }

  return cloneAction(action);
}

export function normalizeMacroRows(rows) {
  if (!Array.isArray(rows)) return [];
  return rows.filter((row) => Array.isArray(row)).map((row) => normalizeAction(row));
}

// 宏指令码 → 中文动作名（文件里存的仍是 kd/ku/md/mu 这类英文码）
const KEY_CMD_LABELS = { kd: '按下', ku: '松开', md: '鼠标按键按下', mu: '鼠标按键松开' };

export function formatActionLabel(action, expanded) {
  const cmd = String(action[0] ?? '').trim().toLowerCase();

  if (cmd === 'wait') {
    return `等待 ${parseNumber(action[1], 0)}ms`;
  }
  if (cmd === 'view') {
    const vec = Array.isArray(action[1]) ? action[1] : [0, 0];
    return `视角 [${parseNumber(vec[0], 0)}, ${parseNumber(vec[1], 0)}] ${Math.max(1, parseNumber(action[2], 50))}ms`;
  }
  if (cmd === 'import') {
    return `调用 ${String(action[1] ?? '').trim() || '（空路径）'}`;
  }
  if (cmd === 'note') {
    const noteText = String(action[1] ?? '').trim();
    const restored = getRecoverableActionFromNoteText(noteText);
    if (restored) {
      return `// 可恢复：${formatActionLabel(restored, false)}`;
    }
    return `// ${noteText}`;
  }
  if (cmd === 'loop') {
    const count = Math.max(1, parseNumber(action[1], 1));
    const block = Array.isArray(action[2]) ? action[2] : [];
    return expanded
      ? `循环 ×${count}（展开，共 ${block.length} 条）`
      : `循环 ×${count}（收起，共 ${block.length} 条）`;
  }
  if (cmd === 'group') {
    const name = String(action[1] ?? '').trim() || '未命名';
    const block = Array.isArray(action[2]) ? action[2] : [];
    return expanded
      ? `宏组「${name}」（展开，共 ${block.length} 条）`
      : `宏组「${name}」（收起，共 ${block.length} 条）`;
  }

  if (['kd', 'ku', 'md', 'mu'].includes(cmd)) {
    return `${KEY_CMD_LABELS[cmd]} '${String(action[1] ?? '')}' ${Math.max(0, parseNumber(action[2], 50))}ms`;
  }

  return JSON.stringify(action);
}

export function getRecoverableActionFromNoteText(text, prefix = RECOVERABLE_NOTE_PREFIX) {
  const raw = String(text ?? '').trim();
  if (!raw.startsWith(prefix)) return null;

  const payload = raw.slice(prefix.length).trim();
  if (!payload) return null;

  try {
    const parsed = JSON.parse(payload);
    if (!Array.isArray(parsed) || !parsed.length) return null;
    if (String(parsed[0] ?? '').trim().toLowerCase() === 'note') return null;
    return normalizeAction(parsed);
  } catch (_err) {
    return null;
  }
}

export function makeRecoverableNoteFromAction(action, prefix = RECOVERABLE_NOTE_PREFIX) {
  return ['note', `${prefix}${JSON.stringify(action)}`];
}
