import { state } from '../core/state.js';
import { q, showToast } from '../core/dom.js';
import { cloneAction } from './action-model.js';
import { setSelectedLine, getActionAtPath } from './action-ops.js';
import { fillActionEditorByAction, ensureActionFormDefaultsForType } from './action-form.js';

/**
 * 动作列表的撤销 / 重做 / 清空。
 *
 * 记录方式：不做「每处改动手动登记」，而是在每次 renderActionList() 之后比对
 * state.editorRows 的结构签名 —— 签名变了才落一个历史点，选择变化/纯 UI 重绘不占步数。
 * 这样块库拖拽、行内编辑、注释、宏组库、录制并进、AI 应用、时间轴移动……全都自动覆盖。
 */
const MAX_STEPS = 80;

const entries = [];   // [{ sig, rows, path, kind }]
let cursor = -1;      // 当前落点：entries[cursor]
let applying = false; // 正在回放历史，别再记一条

function snapshotRows() {
  return cloneAction(Array.isArray(state.editorRows) ? state.editorRows : []);
}

function signature(rows) {
  try {
    return JSON.stringify(rows);
  } catch (err) {
    // 真出现循环引用也别让整个列表挂掉：给个唯一值，宁可多记一步
    return `sig-${Date.now()}-${Math.random()}`;
  }
}

function captureState() {
  const rows = snapshotRows();
  return {
    sig: signature(rows),
    rows,
    path: Array.isArray(state.selectedActionPath) ? [...state.selectedActionPath] : null,
    kind: state.selectedLineKind || 'action',
  };
}

function currentRenderer() {
  return typeof state.renderActionList === 'function' ? state.renderActionList : null;
}

function setDisabled(button, disabled) {
  if (!button) return;
  button.disabled = disabled;
}

/** 按钮可用状态 + 悬停说明 */
export function refreshHistoryButtons() {
  const undoBtn = q('#btnUndoAction');
  const redoBtn = q('#btnRedoAction');
  const undoSteps = Math.max(0, cursor);
  const redoSteps = Math.max(0, entries.length - 1 - cursor);

  setDisabled(undoBtn, undoSteps === 0);
  setDisabled(redoBtn, redoSteps === 0);

  if (undoBtn) {
    undoBtn.title = undoSteps
      ? `撤销上一步改动（快捷键 Ctrl+Z，还能退 ${undoSteps} 步）`
      : '没有可撤销的改动了';
  }
  if (redoBtn) {
    redoBtn.title = redoSteps
      ? `重做被撤销的改动（快捷键 Ctrl+Y，还能前进 ${redoSteps} 步）`
      : '没有可重做的改动了';
  }
}

/** 换文件 / 新建 / 重新加载时调用：历史清空，下一次渲染成为新基线 */
export function resetHistory() {
  entries.length = 0;
  cursor = -1;
  markEditorClean();
  refreshHistoryButtons();
}

/* ── 「有未保存改动」提示 ────────────────────────────────────────────
 * 动作列表改了但没点「保存当前宏」时，触发键跑的还是磁盘上的旧宏 ——
 * 用户很容易以为「刚加的循环没生效」。这里把状态直接写在保存按钮上。
 */
export function refreshSaveButtonState() {
  const btn = q('#btnSaveEditor');
  if (!btn) return;
  const dirty = Boolean(state.editorDirty);
  btn.classList.toggle('unsaved', dirty);
  btn.textContent = dirty ? '保存当前宏 *' : '保存当前宏';
  btn.title = dirty
    ? '动作列表有未保存的改动：点这里写进宏文件，触发键才会用上新的循环/次数'
    : '把动作列表写进当前宏文件（Ctrl+S）';
}

export function markEditorDirty() {
  if (state.editorDirty) return;
  state.editorDirty = true;
  refreshSaveButtonState();
}

export function markEditorClean() {
  state.editorDirty = false;
  refreshSaveButtonState();
}

/** 每次渲染动作列表之后调用；只有动作内容真的变了才落历史点 */
export function syncHistoryFromRows() {
  if (applying) return;

  const now = captureState();
  const top = cursor >= 0 ? entries[cursor] : null;
  // 刚加载/新建出来的第一份内容就是磁盘上的样子，不算「未保存改动」
  const isFirstBaseline = entries.length === 0 && cursor < 0;

  if (top && top.sig === now.sig) {
    // 内容没变（只是换了选中行、切了视图）：只把选中状态同步到当前这一步
    top.path = now.path;
    top.kind = now.kind;
    refreshHistoryButtons();
    return;
  }

  // 有新改动就把「重做」那条分支丢掉
  entries.splice(cursor + 1);
  entries.push(now);
  if (entries.length > MAX_STEPS) entries.shift();
  cursor = entries.length - 1;
  if (!isFirstBaseline) markEditorDirty();
  refreshHistoryButtons();
}

function applyEntry(entry) {
  applying = true;
  try {
    state.editorRows = cloneAction(entry.rows);
    state.inlineEditingPathKey = null;

    let path = null;
    if (Array.isArray(entry.path) && entry.path.length) {
      path = getActionAtPath(entry.path) ? [...entry.path] : null;
    }
    setSelectedLine(path, entry.kind || 'action');

    const action = path ? getActionAtPath(path) : null;
    if (action) fillActionEditorByAction(action);
    else ensureActionFormDefaultsForType(q('#actionType')?.value || 'kd', true);

    const render = currentRenderer();
    if (render) render();
  } finally {
    applying = false;
    refreshHistoryButtons();
  }
}

export function undoAction() {
  if (cursor <= 0) {
    showToast('已经回到最初状态了，没有可撤销的改动');
    refreshHistoryButtons();
    return false;
  }
  cursor -= 1;
  applyEntry(entries[cursor]);
  showToast(`已撤销一步 · 当前 ${entries[cursor].rows.length} 条动作`);
  return true;
}

export function redoAction() {
  if (cursor >= entries.length - 1) {
    showToast('没有可重做的改动了');
    refreshHistoryButtons();
    return false;
  }
  cursor += 1;
  applyEntry(entries[cursor]);
  showToast(`已重做一步 · 当前 ${entries[cursor].rows.length} 条动作`);
  return true;
}

/** 清空整个动作列表（本身是一步历史，能撤销回来） */
export function clearActionList() {
  if (!Array.isArray(state.editorRows) || !state.editorRows.length) {
    showToast('动作列表已经是空的');
    return false;
  }

  const count = state.editorRows.length;
  state.editorRows = [];
  state.inlineEditingPathKey = null;
  state.expandedLoopPathKeys = new Set();
  setSelectedLine(null, 'action');
  ensureActionFormDefaultsForType(q('#actionType')?.value || 'kd', true);

  const render = currentRenderer();
  if (render) render();
  else refreshHistoryButtons();

  showToast(`已清空 ${count} 条动作（Ctrl+Z 可撤销；点「保存当前宏」才写进文件）`);
  return true;
}

function isTypingTarget(target) {
  if (!(target instanceof Element)) return false;
  return Boolean(target.closest('input, textarea, select, [contenteditable="true"], [contenteditable=""]'));
}

function editorTabActive() {
  const page = q('.tab-page[data-tab-page="editor"]');
  return Boolean(page && page.classList.contains('active'));
}

/** 绑按钮 + 键盘（Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y） */
export function bindActionHistory() {
  const undoBtn = q('#btnUndoAction');
  const redoBtn = q('#btnRedoAction');
  const clearBtn = q('#btnClearActionList');

  if (undoBtn) undoBtn.addEventListener('click', () => undoAction());
  if (redoBtn) redoBtn.addEventListener('click', () => redoAction());
  if (clearBtn) clearBtn.addEventListener('click', () => clearActionList());

  window.addEventListener('keydown', (event) => {
    if (!(event.ctrlKey || event.metaKey)) return;
    if (event.altKey) return;
    if (isTypingTarget(event.target)) return; // 输入框里留给浏览器自带的文本撤销
    if (!editorTabActive()) return;

    const key = String(event.key || '').toLowerCase();
    const isUndo = key === 'z' && !event.shiftKey;
    const isRedo = key === 'y' || (key === 'z' && event.shiftKey);
    if (!isUndo && !isRedo) return;

    event.preventDefault();
    if (isUndo) undoAction();
    else redoAction();
  });

  refreshHistoryButtons();
  refreshSaveButtonState();
}
