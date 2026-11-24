import { mountAssistant } from '../assistant-panel.js';
import { state } from '../core/state.js';
import { q } from '../core/dom.js';
import { cloneAction, flattenGroups, formatActionLabel } from './action-model.js';
import { getActionAtPath, setSelectedLine } from './action-ops.js';
import { fillActionEditorByAction, ensureActionFormDefaultsForType } from './action-form.js';
import { renderActionList } from './action-list-render.js';

function appendDiff(diff, rows, type, prefix, start) {
  for (const [index, row] of rows.entries()) {
    const label = prefix + (start + index + 1);
    const marker = type === 'add' ? '+' : '−';
    const cmd = String(row?.[0] ?? '').toLowerCase();
    if (cmd === 'group' && Array.isArray(row[2])) {
      // 宏组在差异里当一层缩进展示，不单独标增删
      appendDiff(diff, row[2], type, label + '.', 0);
      continue;
    }
    diff.push({ type, text: `${marker} ${label}  ${formatActionLabel(row, false)}` });
    if (cmd === 'loop' && Array.isArray(row[2])) {
      appendDiff(diff, row[2], type, label + '.', 0);
    }
  }
}

function rowDiff(before, after) {
  let start = 0;
  let oldEnd = before.length;
  let newEnd = after.length;
  const equal = (left, right) => JSON.stringify(left) === JSON.stringify(right);
  while (start < oldEnd && start < newEnd && equal(before[start], after[start])) start++;
  while (oldEnd > start && newEnd > start && equal(before[oldEnd - 1], after[newEnd - 1])) {
    oldEnd--;
    newEnd--;
  }
  const diff = [];
  appendDiff(diff, before.slice(start, oldEnd), 'remove', '', start);
  appendDiff(diff, after.slice(start, newEnd), 'add', '', start);
  return diff;
}

function replaceSelection(rows, path, replacements) {
  let container = rows;
  for (const index of path.slice(0, -1)) container = container[index][2];
  container.splice(path[path.length - 1], 1, ...replacements);
}

export function bindMacroAssistant() {
  mountAssistant(q('#btnEditorAssistant'), {
    mode: 'macro',
    title: '宏编辑保存',
    emptyTitle: '这次想写什么宏？',
    examples: [
      ['新建快捷键宏', '生成一个 Ctrl+S 组合键，松开后等待 300 ms。', 'new'],
      ['修改选中的动作', '把选中的动作改成等待 80 ms。', 'selection'],
      ['检查按键配对', '检查当前宏里有没有漏松的键，指出对应动作。', 'current'],
    ],
    applyLabel: '应用到动作列表',
    placeholder: '例如，让 Ctrl+S 按下 80 毫秒，松开后等一秒。',
    scopes: [
      ['new', '无 · 生成新动作'],
      ['current', '当前宏'],
      ['selection', '选中的动作'],
    ],
    snapshot() {
      // 宏组只是编辑期的抽屉：给 AI 看的是摊平后的、引擎真的能跑的 rows
      return { path: state.selectedMacro, rows: flattenGroups(state.editorRows) };
    },
    attach(scope, snapshot) {
      if (scope === 'selection') {
        const row = getActionAtPath(state.selectedActionPath);
        if (!row || state.selectedLineKind !== 'action') throw new Error('请先选中一个动作。');
        const flat = flattenGroups([cloneAction(row)]);
        if (!flat.length) throw new Error('选中的宏组里还没有动作。');
        return { data: flat, path: [...state.selectedActionPath] };
      }
      return { data: scope === 'current' ? snapshot.rows : null };
    },
    sameSelection(attached) {
      return !attached.path || state.selectedLineKind === 'action' &&
        JSON.stringify(attached.path) === JSON.stringify(state.selectedActionPath);
    },
    propose(reply, before, attached, scope) {
      const after = { path: before.path, rows: cloneAction(before.rows) };
      if (scope === 'current') after.rows = reply.rows;
      if (scope === 'new') after.rows.push(...reply.rows);
      if (scope === 'selection') replaceSelection(after.rows, attached.path, reply.rows);
      return { after, diff: rowDiff(before.rows, after.rows) };
    },
    stats(reply) {
      const { actions, duration_ms } = reply.stats;
      return `展开 ${actions} 个动作 · ${duration_ms} ms`;
    },
    apply(snapshot) {
      state.editorRows = cloneAction(snapshot.rows);
      state.inlineEditingPathKey = null;
      state.expandedLoopPathKeys.clear();
      const path = getActionAtPath(state.selectedActionPath) ? state.selectedActionPath
        : state.editorRows.length ? [0] : null;
      setSelectedLine(path, 'action');
      const action = getActionAtPath(path);
      if (action) fillActionEditorByAction(action);
      else ensureActionFormDefaultsForType(q('#actionType').value || 'kd', true);
      renderActionList();
    },
  });
}
