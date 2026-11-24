import { state } from '../core/state.js';
import { q, qa, showToast } from '../core/dom.js';
import { apiCall } from '../core/api.js';
import { renderMacroTreeList } from '../macro-tree.js';
import { openImportMacroDialog } from '../macro-import.js';
import {
  ACTION_VIEW_BLOCK,
  ACTION_VIEW_TIMELINE,
  ACTION_VIEW_STORAGE_KEY,
  refreshViewModeButtons,
  readStoredViewMode,
} from './action-xmbc.js';
import { normalizeMacroRows, pathKey, keyToPath, flattenGroups, isContainerAction } from './action-model.js';
import { setSelectedLine, insertActionAtTarget, moveActionsToTarget, resolveInsertTargetAfterSelection, getActionAtPath, clearExpandedLoopKeysByPrefix, deleteActionPaths } from './action-ops.js';
import { fillActionEditorByAction, buildActionFromInputs, ensureActionFormDefaultsForType } from './action-form.js';
import { renderActionList, refreshBatchCommentButton, updateCurrentActionFromEditor, saveGroupToLibraryFromPath, readDraggedPaths, clearMultiSelection } from './action-list-render.js';
import { renderBlockPalette, readPaletteDrag, dropPalettePayload } from './block-palette.js';
import { GROUP_LIB_STORAGE_KEY, invalidateGroupLibrary, hydrateGroupLibrary } from './group-lib.js';
import { bindMacroAssistant } from './assistant.js';
import { bindActionHistory, resetHistory, markEditorClean } from './history.js';
import { attachDragAutoScroll } from '../core/drag-autoscroll.js';

export { renderActionList };
export { loadMacroToEditor, renderEditorMacroBrowser, bindEditorActions };

/** 顶层容器（循环 / 宏组）的路径键：打开宏就先展开，循环体一眼能看见 */
function topLevelContainerKeys(rows) {
  const keys = new Set();
  (Array.isArray(rows) ? rows : []).forEach((row, index) => {
    if (isContainerAction(row)) keys.add(String(index));
  });
  return keys;
}

async function loadMacroToEditor(path) {
  const data = await apiCall('read_macro', path);
  state.editorRows = normalizeMacroRows(data.rows);
  // 打开宏默认把顶层的循环 / 宏组摊开：折叠着的循环看着就跟「没生效」一样
  state.expandedLoopPathKeys = topLevelContainerKeys(state.editorRows);
  state.inlineEditingPathKey = null;
  // 换文件就把多选队列清掉：旧文件里的路径在新文件里可能刚好指到别的动作，留着会误删
  state.selectedActionPaths = [];
  state.multiSelectVisibleCount = 0;

  if (state.editorRows.length) {
    setSelectedLine([0], 'action');
    fillActionEditorByAction(state.editorRows[0]);
  } else {
    setSelectedLine(null, 'action');
    ensureActionFormDefaultsForType(q('#actionType').value || 'kd', true);
  }

  state.selectedMacro = path;
  q('#selectedMacroPath').value = path;
  const { renderMacroList } = await import('../macro-bind.js');
  renderMacroList();
  renderEditorMacroBrowser();
  // 换了一份宏就是新的一段编辑：历史从这份文件当前的样子重新起算
  resetHistory();
  renderActionList();
  renderBlockPalette();
}

function renderEditorMacroBrowser() {
  const list = q('#editorMacroList');
  renderMacroTreeList({
    container: list,
    tree: state.macroTree,
    viewId: 'editor',
    selectedPath: null,
    openedPath: state.selectedMacro,
    onSelect: (path) => {
      loadMacroToEditor(path).catch((err) => showToast(err.message));
    },
  });
}

function restoreActionViewMode() {
  let mode = ACTION_VIEW_BLOCK;
  try {
    mode = readStoredViewMode(window.localStorage);
  } catch (err) {
    /* 隐私模式拿不到 localStorage，退回默认视图 */
  }
  state.actionViewMode = mode;
}

function applyActionViewMode(mode) {
  state.actionViewMode = mode === ACTION_VIEW_TIMELINE ? ACTION_VIEW_TIMELINE : ACTION_VIEW_BLOCK;
  try {
    window.localStorage.setItem(ACTION_VIEW_STORAGE_KEY, state.actionViewMode);
  } catch (err) {
    /* 存不下就算了，本次会话内仍然生效 */
  }
  refreshViewModeButtons(state.actionViewMode);
  renderActionList();
}

/** 左栏「⤓ 存组」：优先存当前选中的宏组，没选中就找序列里第一个宏组 */
async function saveGroupFromPalette() {
  const isGroup = (path) => {
    const action = Array.isArray(path) ? getActionAtPath(path) : null;
    return Array.isArray(action) && String(action[0] ?? '').toLowerCase() === 'group';
  };

  let path = isGroup(state.selectedActionPath) ? state.selectedActionPath : null;

  if (!path) {
    const findFirstGroup = (rows, base = []) => {
      const list = Array.isArray(rows) ? rows : [];
      for (let i = 0; i < list.length; i += 1) {
        const row = list[i];
        if (!Array.isArray(row)) continue;
        const cmd = String(row[0] ?? '').toLowerCase();
        const here = [...base, i];
        if (cmd === 'group') return here;
        if (cmd === 'loop') {
          const found = findFirstGroup(row[2], here);
          if (found) return found;
        }
      }
      return null;
    };
    path = findFirstGroup(state.editorRows);
  }

  if (!path) {
    showToast('还没有宏组可存：先从块库点「宏组」插一个，或把几条动作收进宏组');
    return;
  }

  await saveGroupToLibraryFromPath(path);
}

function bindEditorActions() {
  state.onSelectMacroForEditor = loadMacroToEditor;
  state.renderEditorMacroBrowser = renderEditorMacroBrowser;
  state.renderRecordMacroBrowser = async () => {};
  state.renderActionList = renderActionList;
  state.saveGroupFromPalette = saveGroupFromPalette;

  restoreActionViewMode();
  renderActionList();
  renderBlockPalette();

  // 宏组库真正存在磁盘上（localStorage 只是缓存）：启动时读回来，读完重画左栏
  hydrateGroupLibrary()
    .then(() => renderBlockPalette())
    .catch(() => {});

  bindMacroAssistant();
  bindActionHistory();

  // 另一个窗口改了宏组库（localStorage）时，这边左栏跟着刷新
  window.addEventListener('storage', (e) => {
    if (e.key && e.key !== GROUP_LIB_STORAGE_KEY) return;
    invalidateGroupLibrary();
    renderBlockPalette();
  });

  qa('#actionViewSwitch [data-view-mode]').forEach((btn) => {
    btn.addEventListener('click', () => {
      const next = btn.dataset.viewMode === ACTION_VIEW_TIMELINE ? ACTION_VIEW_TIMELINE : ACTION_VIEW_BLOCK;
      if (next === state.actionViewMode) return;
      applyActionViewMode(next);
      showToast(next === ACTION_VIEW_TIMELINE
        ? '已切到罗技时间轴视图（只改显示，不动宏文件）'
        : '已切回积木块视图');
    });
  });

  q('#btnCopyCode')?.addEventListener('click', async () => {
    const text = q('#timelineCode')?.textContent || '';
    if (!text) return showToast('当前宏是空的');
    try {
      await navigator.clipboard.writeText(text);
      showToast('宏串已复制');
    } catch (err) {
      showToast('复制失败，可手动选中文本复制');
    }
  });

  q('#btnRefreshEditorMacroBrowser').addEventListener('click', async () => {
    const { refreshMacros } = await import('../macro-bind.js');
    await refreshMacros();
    showToast('编辑区宏文件夹已刷新');
  });

  q('#btnEditorImportText')?.addEventListener('click', importRowsIntoEditor);

  // 「录制与化简」在窄窗口会折到下面一行，给个直接跳过去的按钮
  q('#btnJumpToRecord')?.addEventListener('click', () => {
    const target = document.querySelector('.editor-col-side');
    if (!target) return;
    const reduce = window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    target.scrollIntoView({ behavior: reduce ? 'auto' : 'smooth', block: 'start' });
  });

  q('#btnBatchComment')?.addEventListener('click', () => {
    state.batchCommentMode = !state.batchCommentMode;
    if (state.batchCommentMode) {
      state.inlineEditingPathKey = null;
      showToast('批量注释模式已开启：点击动作注释，点击可恢复注释可恢复动作');
    } else {
      showToast('批量注释模式已关闭');
    }
    refreshBatchCommentButton();
    renderActionList();
  });

  q('#btnEditorCreate').addEventListener('click', async () => {
    const folder = q('#editorFolder').value.trim() || '宏/录制宏';
    const fileName = q('#editorNewName').value.trim() || '新建宏.json';
    const ret = await apiCall('create_macro', folder, fileName);
    state.selectedMacro = ret.path;
    state.editorRows = [];
    state.expandedLoopPathKeys = new Set();
    state.inlineEditingPathKey = null;
    state.selectedActionPaths = [];
    state.multiSelectVisibleCount = 0;
    setSelectedLine(null, 'action');
    ensureActionFormDefaultsForType(q('#actionType').value || 'kd', true);
    resetHistory();
    renderActionList();
    const { refreshMacros } = await import('../macro-bind.js');
    await refreshMacros();
    showToast(`已创建：${ret.path}`);
  });

  q('#btnEditorRename').addEventListener('click', async () => {
    if (!state.selectedMacro) return showToast('请先选择一个宏');
    const newName = q('#editorRenameName').value.trim();
    if (!newName) return showToast('请输入新文件名');

    const ret = await apiCall('rename_macro', state.selectedMacro, newName);
    state.selectedMacro = ret.path;
    q('#selectedMacroPath').value = ret.path;
    showToast('重命名完成');
    const { refreshMacros, refreshBindings } = await import('../macro-bind.js');
    await refreshMacros();
    await refreshBindings();
  });

  q('#actionType').addEventListener('change', () => {
    ensureActionFormDefaultsForType(q('#actionType').value, true);
  });

  q('#btnInsertBelow').addEventListener('click', () => {
    try {
      const row = buildActionFromInputs();
      const target = resolveInsertTargetAfterSelection();
      insertActionAtTarget(row, target);
      state.inlineEditingPathKey = pathKey(state.selectedActionPath);
      renderActionList();
    } catch (err) {
      showToast(err.message);
    }
  });

  q('#btnAppendAction').addEventListener('click', () => {
    try {
      const row = buildActionFromInputs();
      insertActionAtTarget(row, { type: 'root', index: state.editorRows.length });
      state.inlineEditingPathKey = pathKey(state.selectedActionPath);
      renderActionList();
    } catch (err) {
      showToast(err.message);
    }
  });

  q('#btnUpdateAction').addEventListener('click', () => {
    try {
      updateCurrentActionFromEditor();
      state.inlineEditingPathKey = pathKey(state.selectedActionPath);
    } catch (err) {
      showToast(err.message);
    }
  });

  // 「删除选中」：多选队列里有什么就一次删完（不是只删最后点的那一条）
  q('#btnDeleteAction').addEventListener('click', () => {
    const paths = state.selectedActionPaths.length
      ? state.selectedActionPaths
      : (Array.isArray(state.selectedActionPath) && state.selectedActionPath.length ? [state.selectedActionPath] : []);

    if (!paths.length) {
      showToast('请先选择一个动作');
      return;
    }

    const result = deleteActionPaths(paths);
    if (!result.count) {
      showToast('选中动作已失效，请重新选择');
      renderActionList();
      return;
    }

    result.paths.forEach((path) => clearExpandedLoopKeysByPrefix(path));
    state.inlineEditingPathKey = null;
    state.selectedActionPaths = [];
    state.multiSelectVisibleCount = 0;

    const nextAction = Array.isArray(result.focusPath) ? getActionAtPath(result.focusPath) : null;
    if (nextAction) {
      setSelectedLine(result.focusPath, 'action');
      fillActionEditorByAction(nextAction);
    } else {
      setSelectedLine(null, 'action');
      ensureActionFormDefaultsForType(q('#actionType').value || 'kd', true);
    }

    renderActionList();
    showToast(result.count > 1
      ? `一次删除了 ${result.count} 条动作（Ctrl+Z 可撤销）`
      : '已删除 1 条动作（Ctrl+Z 可撤销）');
  });

  q('#btnSaveEditor').addEventListener('click', async () => {
    const saveButton = q('#btnSaveEditor');
    // 宏组只是编辑期的抽屉：存盘前摊平成引擎认识的 kd/ku/md/mu/wait/view/loop/import/note
    const rows = flattenGroups(state.editorRows);
    let path = state.selectedMacro;
    if (!path && !rows.length) return showToast('请先在宏列表里选择文件，或添加动作');
    saveButton.disabled = true;
    try {
      if (!path) {
        const folder = q('#editorFolder').value.trim() || '宏';
        const name = q('#editorNewName').value.trim() || '新建宏.json';
        const created = await apiCall('create_macro', folder, name);
        path = created.path;
        if (!state.selectedMacro) {
          state.selectedMacro = path;
          q('#selectedMacroPath').value = path;
        }
      }
      const ret = await apiCall('save_macro', path, rows);
      markEditorClean();
      const synced = Number(ret && ret.cache_refreshed) || 0;
      showToast(synced > 0
        ? `宏文件已保存，并同步了 ${synced} 个触发键的执行缓存：${path}`
        : `宏文件已保存：${path}`);
      const { refreshMacros } = await import('../macro-bind.js');
      await refreshMacros();
    } catch (err) {
      showToast(err.message);
    } finally {
      saveButton.disabled = false;
    }
  });

  q('#btnReloadEditor').addEventListener('click', async () => {
    if (!state.selectedMacro) return showToast('请先选择宏');
    await loadMacroToEditor(state.selectedMacro);
    showToast('已重新加载');
  });

  // Ctrl+S：改完循环/次数立刻落盘，免得触发键还在跑磁盘上的旧宏
  document.addEventListener('keydown', (event) => {
    if (!(event.ctrlKey || event.metaKey) || event.altKey) return;
    if (String(event.key || '').toLowerCase() !== 's') return;
    const page = q('.tab-page[data-tab-page="editor"]');
    if (!page || !page.classList.contains('active')) return;
    event.preventDefault();
    q('#btnSaveEditor')?.click();
  });

  const actionList = q('#actionList');
  if (actionList) {
    // 拖块贴到列表上/下缘就自己滚（原生拖放自动滚动在 WebView2 里不触发）
    attachDragAutoScroll(actionList);

    actionList.addEventListener('dragover', (e) => {
      e.preventDefault();
    });

    actionList.addEventListener('drop', (e) => {
      e.preventDefault();

      // 块库拖进来的块 / 宏组：直接追加到末尾（列表上更精确的位置由每个 li 自己接）
      const payload = readPaletteDrag(e.dataTransfer);
      if (payload) {
        dropPalettePayload(payload, { type: 'root', index: state.editorRows.length });
        return;
      }

      const macroPath = e.dataTransfer.getData('text/macro-path') || e.dataTransfer.getData('text/plain');
      if (macroPath && macroPath.toLowerCase().endsWith('.json')) {
        insertActionAtTarget(['import', macroPath], { type: 'root', index: state.editorRows.length });
        renderActionList();
        showToast(`已追加调用：${macroPath}`);
        return;
      }

      const paths = readDraggedPaths(e.dataTransfer);
      if (paths.length) {
        const moved = moveActionsToTarget(paths, { type: 'root', index: state.editorRows.length }, showToast);
        if (moved) {
          renderActionList();
          if (paths.length > 1) showToast(`已把 ${paths.length} 条动作搬到列表末尾`);
        }
      }

      state.draggedActionPath = null;
      state.draggedActionPaths = [];
    });

    // 点列表空白处＝清空多选队列
    actionList.addEventListener('click', (event) => {
      const target = event.target;
      if (!(target instanceof Element)) return;
      if (target.closest('li')) return;
      if (!state.selectedActionPaths.length) return;
      clearMultiSelection();
    });
  }

  q('#multiSelectClear')?.addEventListener('click', () => {
    clearMultiSelection();
  });

  // Esc：取消多选（对话框开着 / 行内编辑开着时先让它们处理）
  document.addEventListener('keydown', (event) => {
    if (event.key !== 'Escape') return;
    if (document.querySelector('.sd-backdrop')) return;
    if (state.inlineEditingPathKey) return;
    if (!state.selectedActionPaths.length) return;
    clearMultiSelection();
  });

  document.addEventListener('click', (e) => {
    if (!state.inlineEditingPathKey) return;

    const target = e.target;
    if (!(target instanceof Element)) return;

    const editingRow = q(`#actionList li[data-line-key="${state.inlineEditingPathKey}"][data-line-kind="action"]`);
    if (editingRow && editingRow.contains(target)) return;

    const editingPanel = q(`#actionList li[data-inline-editor-for="${state.inlineEditingPathKey}"]`);
    if (editingPanel && editingPanel.contains(target)) return;

    if (target.closest('#actionType')
      || target.closest('#actionArg1')
      || target.closest('#actionArg2')
      || target.closest('#btnUpdateAction')
      || target.closest('#btnInsertBelow')
      || target.closest('#btnAppendAction')
      || target.closest('#btnDeleteAction')) {
      return;
    }

    const editingPath = keyToPath(state.inlineEditingPathKey);
    if (editingPath && editingPath.length) {
      setSelectedLine(editingPath, 'action');
      updateCurrentActionFromEditor({ silent: true });
    }
    state.inlineEditingPathKey = null;
    renderActionList();
  }, true);
}

/* ── 导入现成宏（共用弹窗见 ../macro-import.js） ───────────────────── */

function importRowsIntoEditor() {
  openImportMacroDialog({
    title: '导入现成宏 → 动作列表',
    onRows: ({ rows, warnings }) => {
      const incoming = normalizeMacroRows(rows);
      if (!incoming.length) return;
      const before = state.editorRows.length;
      state.editorRows.push(...incoming);
      setSelectedLine([state.editorRows.length - 1], 'action');
      renderActionList();
      showToast(`已导入 ${incoming.length} 行（${before} → ${state.editorRows.length}）`);
      if (warnings.length) {
        console.warn('[导入现成宏] 告警：', warnings);
      }
    },
  });
}
