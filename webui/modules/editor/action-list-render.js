import { state } from '../core/state.js';
import { q, showToast } from '../core/dom.js';
import {
  pathKey,
  isContainerAction,
  isGroupAction,
  containerKind,
  formatActionLabel,
} from './action-model.js';
import { getActionAtPath, getParentInsertTarget, setSelectedLine, getActionAndContainerByPath, clearExpandedLoopKeysByPrefix, insertActionAtTarget, moveActionsToTarget, normalizeActionPaths } from './action-ops.js';
import { fillActionEditorByAction, buildActionFromInputs } from './action-form.js';
import { toggleCommentForPath } from './comment-ops.js';
import { startInlineEdit, appendInlineEditorRow } from './inline-editor.js';
import { saveGroupToLibrary, hasGroupInLibrary, listGroupFolders, findGroup } from './group-lib.js';
import { openSaveGroupDialog } from './group-editor-dialog.js';
import { renderBlockPalette, readPaletteDrag, dropPalettePayload } from './block-palette.js';
import { syncHistoryFromRows } from './history.js';
import {
  ACTION_VIEW_BLOCK,
  ACTION_VIEW_TIMELINE,
  createChip,
  createArrow,
  describeActionChip,
  updateTimelineCodeBar,
  refreshViewModeButtons,
} from './action-xmbc.js';

const CONTAINER_NAMES = { loop: '循环', group: '宏组' };

export function buildActionRenderLines(actions, parentPath = [], depth = 0, lines = [], ancestry = []) {
  actions.forEach((action, index) => {
    const path = [...parentPath, index];
    if (isContainerAction(action)) {
      const expanded = state.expandedLoopPathKeys.has(pathKey(path));
      const kind = containerKind(action);
      lines.push({ kind: 'action', path, depth, action, isContainerHead: true, container: kind, expanded, ancestry });

      if (expanded) {
        const block = Array.isArray(action[2]) ? action[2] : [];
        const innerAncestry = [...ancestry, kind];
        if (!block.length) {
          lines.push({ kind: 'container_empty', path, depth: depth + 1, action, container: kind, ancestry: innerAncestry });
        } else {
          buildActionRenderLines(block, path, depth + 1, lines, innerAncestry);
        }
        lines.push({ kind: 'container_end', path, depth, action, container: kind, ancestry });
      }
      return;
    }

    lines.push({ kind: 'action', path, depth, action, isContainerHead: false, container: null, expanded: false, ancestry });
  });
  return lines;
}

/* ── 多选：点一下进队、再点一下出队，拖任意一条就整批搬 ─────────── */

/** 同一层里从 anchor 到 target 的一段；跨层就返回 null（退化成单选） */
function rangePathsBetween(anchor, target) {
  if (!Array.isArray(anchor) || !Array.isArray(target)) return null;
  if (!anchor.length || anchor.length !== target.length) return null;

  const parent = anchor.slice(0, -1);
  if (!parent.every((value, index) => value === target[index])) return null;

  const a = anchor[anchor.length - 1];
  const b = target[target.length - 1];
  const [lo, hi] = a <= b ? [a, b] : [b, a];
  const out = [];
  for (let i = lo; i <= hi; i += 1) out.push([...parent, i]);
  return out;
}

/**
 * 点击一行时的选择语义：
 *   普通点＝加入多选队列；再点同一行＝退出队列（不用按修饰键）
 *   Ctrl / Cmd＝只选这一条（清掉其它）
 *   Shift＝从上次那行到这个行选一段
 */
function applyRowSelection(path, kind, event) {
  const exclusive = Boolean(event && (event.ctrlKey || event.metaKey));
  const extending = Boolean(event && event.shiftKey && !exclusive);
  const key = pathKey(path);
  const exists = state.selectedActionPaths.some((item) => pathKey(item) === key);

  if (extending) {
    const picked = rangePathsBetween(state.selectedActionPath, path);
    state.selectedActionPaths = picked || [[...path]];
  } else if (exclusive) {
    state.selectedActionPaths = [[...path]];
  } else if (exists) {
    // 再点一下＝把这个元素从多选队列里拿掉
    state.selectedActionPaths = state.selectedActionPaths.filter((item) => pathKey(item) !== key);
  } else {
    state.selectedActionPaths = [...state.selectedActionPaths, [...path]];
  }

  setSelectedLine(path, kind);
}

/**
 * 时间轴上一块代表哪些动作路径：合并块（1↓1↑ 合成一块）＝按下 + 松开两条。
 * 选中/拖拽必须整对进出，否则搬完之后按下和松开被拆成两块。
 */
function pathsOfTimelineNode(node) {
  if (node && node.merged && Array.isArray(node.altPath) && node.altPath.length) {
    return [[...node.path], [...node.altPath]];
  }
  return node && Array.isArray(node.path) ? [[...node.path]] : [];
}

/**
 * 时间轴方块级点击：合并块整块进出。
 *   普通点＝加入 / 移出；Ctrl＝只留这一块；Shift＝同层选一段。
 */
function applyBlockSelection(paths, primaryPath, kind, event) {
  const list = (Array.isArray(paths) ? paths : []).filter((path) => Array.isArray(path) && path.length);
  if (!list.length) return;

  const exclusive = Boolean(event && (event.ctrlKey || event.metaKey));
  const extending = Boolean(event && event.shiftKey && !exclusive);
  const keys = new Set(list.map((path) => pathKey(path)));
  const anyIn = list.some((path) => state.selectedActionPaths.some((item) => pathKey(item) === pathKey(path)));

  if (extending) {
    const picked = rangePathsBetween(state.selectedActionPath, primaryPath);
    state.selectedActionPaths = picked || list.map((path) => [...path]);
  } else if (exclusive) {
    state.selectedActionPaths = list.map((path) => [...path]);
  } else if (anyIn) {
    // 这一块已经选着了：整块退出（连合并块里那条「松开」一起拿掉）
    state.selectedActionPaths = state.selectedActionPaths.filter((item) => !keys.has(pathKey(item)));
  } else {
    const missing = list.filter((path) => !state.selectedActionPaths.some((item) => pathKey(item) === pathKey(path)));
    state.selectedActionPaths = [...state.selectedActionPaths, ...missing];
  }

  setSelectedLine(primaryPath, kind);
}

/** 选中序号：队列里的先后顺序（拖走时也按这个顺序落） */
function multiOrderMap() {
  const map = new Map();
  state.selectedActionPaths.forEach((path, index) => {
    map.set(pathKey(path), index + 1);
  });
  return map;
}

/** 行左上角那个圆形的「第几个选中」小徽章 */
function makeMultiOrderBadge(order) {
  const badge = document.createElement('span');
  badge.className = 'multi-order';
  badge.textContent = String(order);
  badge.title = `多选队列里的第 ${order} 个`;
  return badge;
}

/** 拖的这条在多选里就整批搬，否则只搬它自己（并顺手丢掉之前的多选） */
function dragPathsFor(path) {
  const key = pathKey(path);
  const inMulti = state.selectedActionPaths.some((item) => pathKey(item) === key);
  if (state.selectedActionPaths.length > 1 && inMulti) {
    return normalizeActionPaths(state.selectedActionPaths);
  }
  if (!inMulti && state.selectedActionPaths.length) {
    state.selectedActionPaths = [];
  }
  return [[...path]];
}

/** 从 dataTransfer / state 读拖拽路径；兼容老的单路径格式 */
export function readDraggedPaths(dataTransfer) {
  const parseList = (text) => {
    if (!text) return null;
    try {
      const value = JSON.parse(text);
      if (!Array.isArray(value) || !value.length) return null;
      if (Array.isArray(value[0])) return value;
      if (typeof value[0] === 'number') return [value];
    } catch (err) {
      /* 不是动作路径，交给别的处理器 */
    }
    return null;
  };

  if (dataTransfer) {
    const multi = parseList(dataTransfer.getData('text/action-paths'));
    if (multi) return multi;
    const single = parseList(dataTransfer.getData('text/action-path'));
    if (single) return single;
  }

  if (Array.isArray(state.draggedActionPaths) && state.draggedActionPaths.length) {
    return state.draggedActionPaths;
  }
  if (Array.isArray(state.draggedActionPath)) return [state.draggedActionPath];
  return [];
}

/** 多选提示条：选到 2 条以上才出现 */
export function refreshMultiSelectBar() {
  const bar = q('#multiSelectBar');
  if (!bar) return;

  const actions = state.selectedActionPaths.length;
  let count = Number.isFinite(state.multiSelectVisibleCount) ? state.multiSelectVisibleCount : actions;
  // 还没渲染过就按动作条数算，免得队列明明有东西、提示条却藏着
  if (count <= 0 && actions > 0) count = actions;
  count = Math.max(0, count);
  bar.hidden = count < 2;

  const text = q('#multiSelectText');
  if (text) {
    const extra = actions > count ? `（共 ${actions} 条动作）` : '';
    text.textContent = `已选 ${count} 项${extra} · 点一下加入、再点一下移出；Ctrl 点击只留这一项；Shift 点击选一段；Esc 或点空白取消。「删除选中」一次删完全部选中项；拖任意一项到宏组 / 循环 / 目标位置，整批一起搬`;
  }
}

export function clearMultiSelection() {
  if (!state.selectedActionPaths.length) return false;
  state.selectedActionPaths = [];
  renderActionList();
  return true;
}

/** 每轮渲染前剔掉已经失效的选中路径（撤销 / 删除之后） */
function pruneMultiSelection() {
  if (!state.selectedActionPaths.length) return;
  state.selectedActionPaths = state.selectedActionPaths.filter((path) => getActionAtPath(path));
}

function getDropTargetFromLine(line) {
  if (line.kind === 'container_end') {
    const container = getActionAtPath(line.path);
    const block = isContainerAction(container) ? container[2] : [];
    return { type: 'container', containerPath: [...line.path], index: block.length };
  }

  if (line.kind === 'container_empty') {
    return { type: 'container', containerPath: [...line.path], index: 0 };
  }

  if (line.kind === 'action' && line.isContainerHead) {
    const body = Array.isArray(line.action?.[2]) ? line.action[2] : [];
    // 拖到宏组 / 循环头上＝收进这个容器：展开的放最前，折叠的放末尾（放完自动展开）
    return { type: 'container', containerPath: [...line.path], index: line.expanded ? 0 : body.length };
  }

  return getParentInsertTarget(line.path, 1);
}

/** 宏组头上的「⤓ 存组」：起名字 + 选文件夹后存进宏组库，左栏立刻能拖 */
async function saveGroupToLibraryFromPath(path) {
  const group = getActionAtPath(path);
  if (!isGroupAction(group)) {
    showToast('这里不是宏组');
    return;
  }

  if (!Array.isArray(group[2]) || !group[2].length) {
    showToast('空宏组没什么可存的，先往里放几条动作');
    return;
  }

  const currentName = String(group[1] ?? '').trim() || '未命名宏组';
  const existing = findGroup(currentName);
  const payload = await openSaveGroupDialog({
    name: currentName,
    folder: existing ? existing.folder : '',
    folders: listGroupFolders(),
    taken: (name) => hasGroupInLibrary(name),
  });
  if (!payload) return;

  // 对话框开着的时候动作可能已经变了，按原路径重新取一次
  const live = getActionAtPath(path);
  if (!isGroupAction(live)) {
    showToast('这个宏组已经不在了，没能存进库');
    return;
  }
  const rows = Array.isArray(live[2]) ? live[2] : [];
  if (!rows.length) {
    showToast('宏组现在是空的，没存');
    return;
  }

  const ret = saveGroupToLibrary(payload.name, rows, {
    folder: payload.folder,
    overwrite: true,
  });
  if (!ret.ok) {
    showToast('没存成：库里已有同名宏组');
    return;
  }

  if (payload.renameInList && payload.name !== currentName) {
    live[1] = payload.name;
    renderActionList();
  }

  renderBlockPalette();
  const where = ret.folder ? `文件夹「${ret.folder}」` : '未分类';
  showToast(ret.persisted
    ? `${ret.updated ? '已更新' : '已存进'}宏组库 ${where}：「${ret.name}」（${rows.length} 条动作），左栏可直接拖用`
    : `宏组库写不进本地存储（可能是隐私模式），本次会话内仍可拖用「${ret.name}」`);
}

function dropRowTargetFromData(e, line) {
  const target = getDropTargetFromLine(line);

  const payload = readPaletteDrag(e.dataTransfer);
  if (payload && dropPalettePayload(payload, target)) {
    return true;
  }

  const macroPath = e.dataTransfer.getData('text/macro-path') || e.dataTransfer.getData('text/plain');
  if (macroPath && macroPath.toLowerCase().endsWith('.json')) {
    insertActionAtTarget(['import', macroPath], target);
    renderActionList();
    showToast(`已插入调用：${macroPath}`);
    return true;
  }

  const paths = readDraggedPaths(e.dataTransfer);
  if (!paths.length) return false;

  const moved = moveActionsToTarget(paths, target, showToast);
  state.draggedActionPath = null;
  state.draggedActionPaths = [];
  if (moved) {
    renderActionList();
    if (paths.length > 1) showToast(`已把 ${paths.length} 条动作搬过去`);
  }
  return true;
}

function updateCurrentActionFromEditor(options = {}) {
  const silent = Boolean(options.silent);
  if (!Array.isArray(state.selectedActionPath) || !state.selectedActionPath.length) {
    if (!silent) showToast('请先在右侧列表中选择一个动作');
    return;
  }

  const info = getActionAndContainerByPath(state.selectedActionPath);
  if (!info) {
    if (!silent) showToast('选中动作已失效，请重新选择');
    return;
  }

  const existing = info.action;
  const fromContainer = isContainerAction(existing);
  const fromKind = containerKind(existing);
  const toType = q('#actionType').value;
  // 循环 ↔ 宏组 互转时容器体是带着走的，真正会丢内容的只有「容器 → 普通动作」
  const toIsContainer = toType === 'loop' || toType === 'group';

  if (fromContainer && !toIsContainer) {
    if (silent) {
      return;
    }
    const label = CONTAINER_NAMES[fromKind] || '容器';
    const ok = window.confirm(`当前选中的是${label}动作，改成其他类型会丢掉${label}体内的动作。\n\n点「确定」继续（${label}体内的动作会被清空）；\n点「取消」保持${label}动作不变。`);
    if (!ok) return;
  }

  const next = buildActionFromInputs({ existingAction: existing });
  info.container[info.index] = next;

  if (fromContainer && !isContainerAction(next)) {
    clearExpandedLoopKeysByPrefix(state.selectedActionPath);
  }

  renderActionList();
  if (!silent) showToast('当前动作已修改');
}

export function renderActionList() {
  const list = q('#actionList');
  pruneMultiSelection();
  list.innerHTML = '';

  const viewMode = state.actionViewMode === ACTION_VIEW_TIMELINE ? ACTION_VIEW_TIMELINE : ACTION_VIEW_BLOCK;
  list.classList.toggle('timeline-mode', viewMode === ACTION_VIEW_TIMELINE);

  if (viewMode === ACTION_VIEW_TIMELINE) {
    renderTimelineFlow(list);
  } else {
    renderBlockRows(list);
  }

  updateTimelineCodeBar(state.editorRows, viewMode);
  refreshViewModeButtons(viewMode);
  // 提示条按「当前视图里亮了几个方块」算：时间轴上合并块算一项（两条动作）
  state.multiSelectVisibleCount = list.querySelectorAll('li.multi-selected').length;
  refreshMultiSelectBar();
  // 撤销/重做靠这里自动记步：内容真变了才落历史点
  syncHistoryFromRows();
}

/* ── 积木块视图：一行一条动作 ───────────────────────────────────── */

function renderBlockRows(list) {
  const lines = buildActionRenderLines(state.editorRows, [], 0, []);

  if (!lines.length) {
    const empty = document.createElement('li');
    empty.className = 'action-list-empty';
    const strong = document.createElement('b');
    strong.textContent = '还没有动作';
    const tip = document.createElement('span');
    tip.textContent = '从左栏块库拖一个块进来，或者点一下块（会加到这里）';
    empty.appendChild(strong);
    empty.appendChild(tip);
    list.appendChild(empty);
    return;
  }

  const multiOrder = multiOrderMap();

  lines.forEach((line, lineIndex) => {
    const li = document.createElement('li');
    const lineKey = pathKey(line.path);
    const order = multiOrder.get(lineKey);
    const active = Array.isArray(state.selectedActionPath)
      && pathKey(state.selectedActionPath) === lineKey
      && state.selectedLineKind === line.kind;

    li.classList.toggle('active', active);
    li.classList.toggle('multi-selected', Boolean(order));
    li.classList.toggle('editing', !state.batchCommentMode && line.kind === 'action' && state.inlineEditingPathKey === lineKey);
    if (line.kind === 'action' && line.isContainerHead) {
      li.classList.add('container-head');
      li.classList.add(line.container === 'group' ? 'container-group' : 'container-loop');
    }
    if (line.kind === 'container_end') {
      li.classList.add('container-end');
      if (line.container === 'group') li.classList.add('container-group');
    }
    if (line.kind === 'container_empty') {
      li.classList.add('container-empty');
      if (line.container === 'group') li.classList.add('container-group');
    }
    const ancestry = Array.isArray(line.ancestry) ? line.ancestry : [];
    if (ancestry.includes('group')) li.classList.add('in-group');
    if (ancestry.includes('loop')) li.classList.add('in-loop');
    li.dataset.lineKey = lineKey;
    li.dataset.lineKind = line.kind;
    if (line.container) li.dataset.container = line.container;

    li.draggable = line.kind === 'action' || line.kind === 'container_end';

    li.addEventListener('click', (event) => {
      if (state.batchCommentMode && line.kind === 'action') {
        state.inlineEditingPathKey = null;
        const ok = toggleCommentForPath(line.path, { silent: false });
        if (ok) {
          renderActionList();
        }
        return;
      }

      applyRowSelection(line.path, line.kind, event);
      const selected = getActionAtPath(line.path);
      if (selected) fillActionEditorByAction(selected);
      renderActionList();
    });

    li.addEventListener('dblclick', (e) => {
      if (state.batchCommentMode) return;
      if (line.kind !== 'action') return;
      if (e.target instanceof Element && e.target.closest('.loop-toggle')) return;
      if (e.target instanceof Element && e.target.closest('.group-save')) return;
      e.stopPropagation();
      startInlineEdit(line.path, renderActionList);
    });

    li.addEventListener('dragstart', (e) => {
      const paths = dragPathsFor(line.path);
      state.draggedActionPaths = paths;
      state.draggedActionPath = paths[0];
      e.dataTransfer.effectAllowed = 'move';
      e.dataTransfer.setData('text/action-paths', JSON.stringify(paths));
      e.dataTransfer.setData('text/action-path', JSON.stringify(paths[0]));
    });

    li.addEventListener('dragend', () => {
      state.draggedActionPath = null;
      state.draggedActionPaths = [];
      li.classList.remove('drag-over');
    });

    li.addEventListener('dragover', (e) => {
      e.preventDefault();
      li.classList.add('drag-over');
    });

    li.addEventListener('dragleave', () => {
      li.classList.remove('drag-over');
    });

    li.addEventListener('drop', (e) => {
      e.preventDefault();
      // 别让事件继续冒到 #actionList 的「追加到末尾」处理器，否则一次拖放会插两遍
      e.stopPropagation();
      li.classList.remove('drag-over');
      dropRowTargetFromData(e, line);
    });

    const idxEl = document.createElement('span');
    idxEl.className = 'action-index';
    idxEl.textContent = String(lineIndex + 1);

    const textWrap = document.createElement('span');
    textWrap.className = 'action-text';

    const indent = document.createElement('span');
    indent.className = 'action-indent';
    indent.style.width = `${line.depth * 14}px`;
    textWrap.appendChild(indent);

    if (line.kind === 'action' && line.isContainerHead) {
      const toggle = document.createElement('button');
      toggle.type = 'button';
      toggle.className = 'loop-toggle';
      toggle.textContent = line.expanded ? '▼' : '▶';
      toggle.addEventListener('click', (e) => {
        e.stopPropagation();
        toggleExpandedLoop(line.path);
      });
      toggle.addEventListener('dblclick', (e) => {
        e.stopPropagation();
        toggleExpandedLoop(line.path);
      });
      textWrap.appendChild(toggle);
      textWrap.appendChild(document.createTextNode(formatActionLabel(line.action, line.expanded)));
    } else if (line.kind === 'container_end') {
      const name = String(line.action?.[1] ?? '').trim();
      const label = CONTAINER_NAMES[line.container] || '容器';
      textWrap.appendChild(document.createTextNode(
        line.container === 'group' && name ? `↳ 结束宏组「${name}」` : `↳ 结束${label}`,
      ));
    } else if (line.kind === 'container_empty') {
      const label = CONTAINER_NAMES[line.container] || '容器';
      textWrap.appendChild(document.createTextNode(`（${label}体内暂无动作，从左侧块库拖块到这里）`));
    } else {
      textWrap.appendChild(document.createTextNode(formatActionLabel(line.action, false)));
    }

    li.appendChild(idxEl);
    li.appendChild(textWrap);
    if (order) li.appendChild(makeMultiOrderBadge(order));

    if (line.kind === 'action' && line.isContainerHead && line.container === 'group') {
      li.classList.add('has-actions');
      const saveBtn = document.createElement('button');
      saveBtn.type = 'button';
      saveBtn.className = 'group-save';
      saveBtn.textContent = '⤓ 存组';
      saveBtn.title = '存进宏组库：可以起名字、选文件夹；左栏「我的宏组」随后可以直接拖用';
      saveBtn.addEventListener('click', (e) => {
        e.stopPropagation();
        saveGroupToLibraryFromPath(line.path);
      });
      li.appendChild(saveBtn);
    }

    list.appendChild(li);

    if (!state.batchCommentMode && line.kind === 'action' && state.inlineEditingPathKey === lineKey) {
      appendInlineEditorRow(list, line, lineKey, {
        toggleCommentForPath,
        renderActionList,
        updateCurrentActionFromEditor,
      });
    }
  });
}

/* ── 罗技 / 拼宏式方块流：整片横向排布，块与块之间 → ───────────── */

function toggleExpandedLoop(path) {
  const key = pathKey(path);
  if (state.expandedLoopPathKeys.has(key)) {
    state.expandedLoopPathKeys.delete(key);
  } else {
    state.expandedLoopPathKeys.add(key);
  }
  renderActionList();
}

function buildFlowNodes(actions, parentPath = [], out = [], ancestry = []) {
  (Array.isArray(actions) ? actions : []).forEach((action, index) => {
    const path = [...parentPath, index];

    if (isContainerAction(action)) {
      const expanded = state.expandedLoopPathKeys.has(pathKey(path));
      const body = Array.isArray(action[2]) ? action[2] : [];
      const kind = containerKind(action);
      out.push({ kind: 'container', container: kind, path, action, expanded, ancestry, spec: describeActionChip(action, expanded) });

      if (expanded) {
        const label = CONTAINER_NAMES[kind] || '容器';
        if (!body.length) {
          out.push({
            kind: 'containerEmpty', container: kind, path, action, ancestry,
            spec: { main: `空${label}体`, sub: '可拖动作进来', tone: 'muted', title: `${label}体内暂无动作` },
          });
        } else {
          buildFlowNodes(body, path, out, [...ancestry, kind]);
        }
        out.push({
          kind: 'containerEnd', container: kind, path, action, ancestry,
          spec: { main: `结束${label}`, sub: '', tone: 'muted', title: `${label}体到此结束` },
        });
      }
      return;
    }

    out.push({ kind: 'action', path, action, ancestry, spec: describeActionChip(action, false) });

    const cmd = String(action[0] ?? '').toLowerCase();
    const delay = Number(action[2]);
    if ((cmd === 'kd' || cmd === 'ku' || cmd === 'md' || cmd === 'mu') && Number.isFinite(delay) && delay > 0) {
      out.push({
        kind: 'delay', path, action, ancestry,
        spec: { main: `${Math.round(delay * 1000) / 1000}ms`, sub: '', tone: 'delay', title: '动作后的延迟' },
      });
    }
  });
  return out;
}

/**
 * 同一键的「按下 ↓」后面紧跟「松开 ↑」、中间没有任何延迟 → 判为一对。
 * 只要两者之间夹了延迟（那会先生成一个独立的延迟块），或者不同键、不同容器，就不算。
 */
function holdReleasePairOf(a, b) {
  if (a.kind !== 'action' || b.kind !== 'action') return null;

  const cmdA = String(a.action?.[0] ?? '').toLowerCase();
  const cmdB = String(b.action?.[0] ?? '').toLowerCase();
  if (cmdA === 'kd' && cmdB !== 'ku') return null;
  if (cmdA === 'md' && cmdB !== 'mu') return null;
  if (cmdA !== 'kd' && cmdA !== 'md') return null;

  // 必须是同一个父容器里的前后两条，跨容器 / 跨循环体不合并
  const sameParent = a.path.length === b.path.length
    && a.path.length > 0
    && a.path.slice(0, -1).every((value, index) => value === b.path[index])
    && b.path[b.path.length - 1] === a.path[a.path.length - 1] + 1;
  if (!sameParent) return null;

  const fallback = cmdA === 'kd' ? '' : 'left';
  const nameA = String(a.action?.[1] ?? fallback).trim().toLowerCase();
  const nameB = String(b.action?.[1] ?? fallback).trim().toLowerCase();
  if (nameA !== nameB) return null;

  return { name: nameA };
}

/**
 * 时间轴显示层：把「按下+松开」且中间没有延迟的一对合成一块，只写键名（不写 ↓/↑）。
 * 只动渲染后的节点流，动作数据、宏代码、撤销历史都不变。
 */
function mergeHoldReleaseNodes(nodes) {
  const out = [];
  for (let i = 0; i < nodes.length; i += 1) {
    const pair = i + 1 < nodes.length ? holdReleasePairOf(nodes[i], nodes[i + 1]) : null;
    if (!pair) {
      out.push(nodes[i]);
      continue;
    }

    const first = nodes[i];
    const second = nodes[i + 1];
    out.push({
      ...first,
      merged: true,
      altPath: second.path,
      spec: {
        ...first.spec,
        sub: '',
        title: `${first.spec.title} ＋ ${second.spec.title}（中间没有延迟，已合并成一块）`,
      },
    });
    i += 1; // 跳过紧跟的松开块
  }
  return out;
}

function flowDropTarget(node) {
  if (node.kind === 'containerEnd' || node.kind === 'containerEmpty') {
    const container = getActionAtPath(node.path);
    const block = isContainerAction(container) ? container[2] : [];
    return { type: 'container', containerPath: [...node.path], index: block.length };
  }

  if (node.kind === 'container') {
    // 拖到宏组 / 循环头上＝收进这个容器：展开的放最前，折叠的放末尾（放完自动展开）
    const body = Array.isArray(node.action?.[2]) ? node.action[2] : [];
    return { type: 'container', containerPath: [...node.path], index: node.expanded ? 0 : body.length };
  }

  const parentPath = node.path.slice(0, -1);
  const index = node.path[node.path.length - 1] + (node.kind === 'delay' ? 1 : 0);
  return parentPath.length
    ? { type: 'container', containerPath: parentPath, index }
    : { type: 'root', index };
}

function renderTimelineFlow(list) {
  const nodes = mergeHoldReleaseNodes(buildFlowNodes(state.editorRows));
  const selectedKeys = new Set(state.selectedActionPaths.map((path) => pathKey(path)));
  let blockOrder = 0;

  if (!nodes.length) {
    const empty = document.createElement('li');
    empty.className = 'tl-node tl-empty';
    empty.appendChild(createChip('空宏', '从左侧块库拖一个块进来', 'muted', '当前宏没有任何动作'));
    list.appendChild(empty);
    return;
  }

  const inlineCallbacks = {
    toggleCommentForPath,
    renderActionList,
    updateCurrentActionFromEditor,
  };

  nodes.forEach((node, index) => {
    if (index > 0) {
      const arrowLi = document.createElement('li');
      arrowLi.className = 'tl-arrow-node';
      arrowLi.appendChild(createArrow());
      list.appendChild(arrowLi);
    }

    const li = document.createElement('li');
    li.className = `tl-node tl-${node.kind}${node.container ? ` tl-${node.container}-node` : ''}`;
    // 循环 / 宏组体内的块带个标记：时间轴上看得清「这些块是被循环包着的」
    const ancestry = Array.isArray(node.ancestry) ? node.ancestry : [];
    if (ancestry.includes('loop')) li.classList.add('tl-in-loop');
    if (ancestry.includes('group')) li.classList.add('tl-in-group');
    li.dataset.path = pathKey(node.path);
    li.dataset.kind = node.kind;
    if (node.container) li.dataset.container = node.container;
    li.title = node.spec.title || '';

    const isActionLike = node.kind === 'action' || node.kind === 'container';
    const active = isActionLike
      && Array.isArray(state.selectedActionPath)
      && pathKey(state.selectedActionPath) === pathKey(node.path)
      && state.selectedLineKind === 'action';
    li.classList.toggle('active', active);

    // 合并块（1↓1↑）只要有一条在队列里就整块点亮，序号按「块」连续编
    const nodePaths = node.kind === 'delay' ? [] : pathsOfTimelineNode(node);
    const selectedBlock = nodePaths.some((path) => selectedKeys.has(pathKey(path)));
    const multiOrderNo = selectedBlock ? (blockOrder += 1) : 0;
    li.classList.toggle('multi-selected', selectedBlock);

    if (node.kind === 'container') {
      const toggle = document.createElement('button');
      toggle.type = 'button';
      toggle.className = 'loop-toggle';
      toggle.textContent = node.expanded ? '▼' : '▶';
      toggle.addEventListener('click', (e) => {
        e.stopPropagation();
        toggleExpandedLoop(node.path);
      });
      toggle.addEventListener('dblclick', (e) => {
        e.stopPropagation();
        toggleExpandedLoop(node.path);
      });
      li.appendChild(toggle);
    }

    li.appendChild(createChip(node.spec.main, node.spec.sub, node.spec.tone, node.spec.title, node.spec.mouse));

    if (node.kind === 'container' && node.container === 'group') {
      const saveBtn = document.createElement('button');
      saveBtn.type = 'button';
      saveBtn.className = 'group-save group-save-chip';
      saveBtn.textContent = '⤓';
      saveBtn.title = '存进宏组库：可以起名字、选文件夹；左栏可以直接拖用';
      saveBtn.addEventListener('click', (e) => {
        e.stopPropagation();
        saveGroupToLibraryFromPath(node.path);
      });
      saveBtn.addEventListener('dblclick', (e) => e.stopPropagation());
      li.appendChild(saveBtn);
    }

    if (multiOrderNo) li.appendChild(makeMultiOrderBadge(multiOrderNo));

    if (isActionLike) li.draggable = true;

    li.addEventListener('click', (event) => {
      if (node.kind === 'containerEnd' || node.kind === 'containerEmpty') return;

      // 延迟块只是那条动作的尾巴：点它只换主选中，不往多选队列里加减
      if (node.kind === 'delay') {
        setSelectedLine(node.path, 'action');
        renderActionList();
        return;
      }

      if (state.batchCommentMode && isActionLike) {
        state.inlineEditingPathKey = null;
        if (toggleCommentForPath(node.path, { silent: false })) {
          renderActionList();
        }
        return;
      }

      applyBlockSelection(pathsOfTimelineNode(node), node.path, 'action', event);
      const selected = getActionAtPath(node.path);
      if (selected) fillActionEditorByAction(selected);
      renderActionList();
    });

    li.addEventListener('dblclick', (e) => {
      if (state.batchCommentMode) return;
      if (node.kind === 'containerEnd' || node.kind === 'containerEmpty') return;
      if (e.target instanceof Element && e.target.closest('.loop-toggle')) return;
      if (e.target instanceof Element && e.target.closest('.group-save')) return;
      e.stopPropagation();
      startInlineEdit(node.path, renderActionList);
    });

    li.addEventListener('dragstart', (e) => {
      if (!isActionLike) return;
      let paths = dragPathsFor(node.path);

      // 拖的是合并块（1↓1↑ 合成一块）：整对一起走，绝不把按下和松开拆散
      const pair = pathsOfTimelineNode(node);
      if (pair.length > 1) {
        const pairKeys = new Set(pair.map((path) => pathKey(path)));
        const hasAll = [...pairKeys].every((key) => paths.some((path) => pathKey(path) === key));
        if (!hasAll) {
          paths = [...paths.filter((path) => !pairKeys.has(pathKey(path))), ...pair];
        }
      }

      state.draggedActionPaths = paths;
      state.draggedActionPath = paths[0];
      e.dataTransfer.effectAllowed = 'move';
      e.dataTransfer.setData('text/action-paths', JSON.stringify(paths));
      e.dataTransfer.setData('text/action-path', JSON.stringify(paths[0]));
    });

    li.addEventListener('dragend', () => {
      state.draggedActionPath = null;
      state.draggedActionPaths = [];
      li.classList.remove('drag-over');
    });

    li.addEventListener('dragover', (e) => {
      e.preventDefault();
      li.classList.add('drag-over');
    });

    li.addEventListener('dragleave', () => {
      li.classList.remove('drag-over');
    });

    li.addEventListener('drop', (e) => {
      e.preventDefault();
      e.stopPropagation();
      li.classList.remove('drag-over');

      const target = flowDropTarget(node);
      const payload = readPaletteDrag(e.dataTransfer);
      if (payload && dropPalettePayload(payload, target)) return;

      const macroPath = e.dataTransfer.getData('text/macro-path') || e.dataTransfer.getData('text/plain');
      if (macroPath && macroPath.toLowerCase().endsWith('.json')) {
        insertActionAtTarget(['import', macroPath], target);
        renderActionList();
        showToast(`已插入调用：${macroPath}`);
        return;
      }

      const paths = readDraggedPaths(e.dataTransfer);
      if (!paths.length) return;

      const moved = moveActionsToTarget(paths, target, showToast);
      state.draggedActionPath = null;
      state.draggedActionPaths = [];
      if (moved) {
        renderActionList();
        if (paths.length > 1) showToast(`已把 ${paths.length} 条动作搬过去`);
      }
    });

    list.appendChild(li);

    const editingKey = pathKey(node.path);
    if (!state.batchCommentMode && state.inlineEditingPathKey === editingKey && isActionLike) {
      const selectedAction = getActionAtPath(node.path);
      if (selectedAction) {
        appendInlineEditorRow(list, {
          path: node.path,
          action: selectedAction,
          kind: 'action',
          depth: 0,
          isContainerHead: false,
          expanded: Boolean(node.expanded),
        }, editingKey, inlineCallbacks);
      }
    }
  });
}

export function refreshBatchCommentButton() {
  const btn = q('#btnBatchComment');
  const list = q('#actionList');
  if (list) list.classList.toggle('batch-comment-mode', state.batchCommentMode);
  if (!btn) return;
  btn.textContent = `批量注释：${state.batchCommentMode ? '开' : '关'}`;
  btn.classList.toggle('active', state.batchCommentMode);
}

export { updateCurrentActionFromEditor, saveGroupToLibraryFromPath };
