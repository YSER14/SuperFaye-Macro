// -*- coding: utf-8 -*-
/**
 * 动作块库（左栏）——照 XMBC 拼宏编辑器的积木块面板搬过来的拖拽拼宏入口。
 *
 * 和老的「动作插入器」表单的区别：
 *   * 不是先选类型、填参数、再点按钮，而是把块**拖**到序列里你要的位置；
 *   * 点一下块 = 插到当前选中动作的下面（没选中就加到末尾），照样不用填表；
 *   * 每种块自带一份合理默认值，插进去以后再双击就地改参数。
 *
 * 块 → 宏行的映射（宏文件里存的仍是 kd / ku / md / mu / wait / view / loop / import / note）：
 *   延迟 → ['wait', ms]          按下 → ['kd', key, ms]       松开 → ['ku', key, ms]
 *   敲一下 → kd + ku 两行          鼠标按下/松开 → md / mu      视角移动 → ['view',[dx,dy],ms]
 *   循环 → ['loop', n, []]        宏组 → ['group', 名字, []]   调用子宏 → ['import', 路径]
 *   注释 → ['note', 文本]
 */
import { state } from '../core/state.js';
import { q, showToast } from '../core/dom.js';
import { isContainerAction, pathKey } from './action-model.js';
import { getActionAtPath, getParentInsertTarget, insertRowsAtTarget } from './action-ops.js';
import { fillActionEditorByAction } from './action-form.js';
import { groupLibraryEntries, rowsFromLibrary, deleteGroupFromLibrary, listGroupFolders, createGroupFolder, deleteGroupFolder, updateGroupInLibrary, hasGroupInLibrary } from './group-lib.js';
import { openEditGroupDialog, openNewFolderDialog, openGroupNameDialog } from './group-editor-dialog.js';

export const BLOCK_CATALOG = [
  {
    group: '时序',
    items: [
      {
        type: 'wait',
        label: '延迟',
        icon: '⏱',
        hint: '与下一条动作之间的等待',
        make: () => [['wait', 100]],
      },
    ],
  },
  {
    group: '键盘',
    items: [
      {
        type: 'kd',
        label: '按下',
        icon: '⬇',
        hint: '按住不放，必须配一个松开',
        make: () => [['kd', 'w', 50]],
      },
      {
        type: 'ku',
        label: '松开',
        icon: '⬆',
        hint: '松开按键',
        make: () => [['ku', 'w', 50]],
      },
      {
        type: 'tap',
        label: '敲一下',
        icon: '⌨',
        hint: '按下再松开（两行一起插入）',
        make: () => [['kd', 'w', 50], ['ku', 'w', 0]],
      },
    ],
  },
  {
    group: '鼠标',
    items: [
      {
        type: 'md',
        label: '鼠标按下',
        icon: '🖱',
        hint: '左键按住不放，可拖视角',
        make: () => [['md', 'left', 50]],
      },
      {
        type: 'mu',
        label: '鼠标松开',
        icon: '🖱',
        hint: '松开鼠标键',
        make: () => [['mu', 'left', 0]],
      },
      {
        type: 'view',
        label: '视角移动',
        icon: '🎯',
        hint: '按 dx,dy 转视角，带时长',
        make: () => [['view', [0, 2], 50]],
      },
    ],
  },
  {
    group: '流程',
    items: [
      {
        type: 'loop',
        label: '循环',
        icon: '🔁',
        hint: '把里面那组动作重复 N 次',
        make: () => [['loop', 3, []]],
      },
      {
        type: 'group',
        label: '宏组',
        icon: '🗂',
        hint: '打包一组块，纯组织用途；存盘时摊平',
        make: () => [['group', '新组', []]],
      },
      {
        type: 'import',
        label: '调用子宏',
        icon: '🔗',
        hint: '调用另一个宏文件（会插成 import 行）',
        make: () => [['import', '宏/录制宏/示例.json']],
      },
      {
        type: 'note',
        label: '注释',
        icon: '//',
        hint: '只写在编辑器里，不影响执行',
        make: () => [['note', '备注']],
      },
    ],
  },
];

export const PALETTE_BLOCK_MIME = 'text/block-type';
export const PALETTE_LIB_MIME = 'text/block-library';

const BLOCK_INDEX = new Map();
BLOCK_CATALOG.forEach((cat) => cat.items.forEach((item) => BLOCK_INDEX.set(item.type, item)));

export function getPaletteBlock(type) {
  return BLOCK_INDEX.get(String(type || '')) || null;
}

function currentListRenderer() {
  return typeof state.renderActionList === 'function' ? state.renderActionList : () => {};
}

/** 点块时插哪：选中容器 → 容器末尾；选中动作 → 它的下面；什么都没选 → 整个宏末尾 */
export function resolvePaletteInsertTarget() {
  const path = state.selectedActionPath;
  if (!Array.isArray(path) || !path.length) {
    return { type: 'root', index: state.editorRows.length };
  }

  const action = getActionAtPath(path);
  if (!action) {
    // 选中的那一行已经被删了：退回整条宏末尾，别把块插到不存在的容器里
    return { type: 'root', index: state.editorRows.length };
  }

  if (isContainerAction(action)) {
    return { type: 'container', containerPath: [...path], index: action[2].length };
  }

  return getParentInsertTarget(path, 1);
}

function expandIfContainer(path) {
  if (!Array.isArray(path) || !path.length) return;
  const action = getActionAtPath(path);
  if (isContainerAction(action)) state.expandedLoopPathKeys.add(pathKey(path));
}

/**
 * 把块库里的一个块插进序列。
 * @param {string} type 块类型（wait/kd/ku/tap/md/mu/view/loop/group/import/note）
 * @param {object} [target] 不传就走 resolvePaletteInsertTarget()
 * @returns {Array|null} 最后一行的路径
 */
export function insertPaletteBlock(type, target) {
  const item = getPaletteBlock(type);
  if (!item) {
    showToast(`不认识的块：${type}`);
    return null;
  }

  const rows = item.make();
  let lastPath = null;
  try {
    lastPath = insertRowsAtTarget(rows, target || resolvePaletteInsertTarget());
  } catch (err) {
    showToast(err.message || '插不进去：插入位置失效了，点一下别的行再试');
    return null;
  }
  if (lastPath) {
    expandIfContainer(lastPath);
    const inserted = getActionAtPath(lastPath);
    if (inserted) fillActionEditorByAction(inserted);
  }
  currentListRenderer()();

  // 「调用子宏」的路径没法猜：插完直接把它选中、把光标放进子宏路径，让用户接着敲
  if (type === 'import') {
    const input = q('#actionArg1');
    if (input) {
      input.focus();
      input.select();
    }
    showToast('已插入「调用子宏」：把块参数里的子宏路径改成你的 .json');
  }

  return lastPath;
}

/** 给新宏组起个不重名的默认名：宏组1、宏组2… */
function suggestGroupName() {
  const names = new Set();
  const walk = (rows) => {
    (Array.isArray(rows) ? rows : []).forEach((row) => {
      if (!Array.isArray(row)) return;
      const cmd = String(row[0] ?? '').toLowerCase();
      if (cmd === 'group' || cmd === 'loop') {
        if (cmd === 'group') names.add(String(row[1] ?? '').trim());
        walk(row[2]);
      }
    });
  };
  walk(state.editorRows);

  let index = 1;
  while (names.has(`宏组${index}`)) index += 1;
  return `宏组${index}`;
}

/**
 * 插入一个宏组：先弹命名框，确认后再插。
 * 拖拽和点击都走这里，保证「命名」这一步不会被绕过。
 */
async function insertNamedGroupBlock(target) {
  const payload = await openGroupNameDialog({ name: suggestGroupName() });
  if (!payload) return null;

  const rows = [['group', payload.name, []]];
  let lastPath = null;
  try {
    lastPath = insertRowsAtTarget(rows, target || resolvePaletteInsertTarget());
  } catch (err) {
    showToast(err.message || '插不进去：插入位置失效了，点一下别的行再试');
    return null;
  }

  if (lastPath) {
    expandIfContainer(lastPath);
    const inserted = getActionAtPath(lastPath);
    if (inserted) fillActionEditorByAction(inserted);
  }
  currentListRenderer()();
  showToast(`已插入宏组「${payload.name}」，往里面拖块就行`);
  return lastPath;
}

/** 从宏组库插一整组（真实动作，不是引用） */
export function insertLibraryGroup(name, groupPath = '', target) {
  const groupRow = rowsFromLibrary(name, groupPath);
  if (!groupRow) {
    showToast('这条宏组已经不在库里了');
    renderBlockPalette();
    return null;
  }

  let lastPath = null;
  try {
    lastPath = insertRowsAtTarget([groupRow], target || resolvePaletteInsertTarget());
  } catch (err) {
    showToast(err.message || '插不进去：插入位置失效了，点一下别的行再试');
    return null;
  }
  expandIfContainer(lastPath);
  const inserted = lastPath ? getActionAtPath(lastPath) : null;
  if (inserted) fillActionEditorByAction(inserted);
  currentListRenderer()();
  showToast(`已插入宏组「${groupRow[1]}」（${groupRow[2].length} 条动作）`);
  return lastPath;
}

/* ── 渲染左栏 ─────────────────────────────────────────────────────────── */

function makePaletteBlock(item) {
  const el = document.createElement('div');
  el.className = 'palette-block';
  el.draggable = true;
  el.dataset.blockType = item.type;
  el.setAttribute('role', 'button');
  el.tabIndex = 0;
  el.title = `${item.label} — ${item.hint}\n拖到右侧列表的任意位置，或点一下插到选中动作下面`;

  const icon = document.createElement('span');
  icon.className = 'pb-icon';
  icon.textContent = item.icon;

  const text = document.createElement('span');
  text.className = 'pb-text';
  const name = document.createElement('b');
  name.textContent = item.label;
  const hint = document.createElement('small');
  hint.textContent = item.hint;
  text.appendChild(name);
  text.appendChild(hint);

  el.appendChild(icon);
  el.appendChild(text);

  el.addEventListener('click', () => {
    if (item.type === 'group') {
      void insertNamedGroupBlock();
      return;
    }
    insertPaletteBlock(item.type);
  });

  el.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      if (item.type === 'group') {
        void insertNamedGroupBlock();
        return;
      }
      insertPaletteBlock(item.type);
    }
  });

  el.addEventListener('dragstart', (e) => {
    e.dataTransfer.effectAllowed = 'copy';
    e.dataTransfer.setData(PALETTE_BLOCK_MIME, item.type);
    // 兜底：别的 drop 目标（比如文件夹输入框）至少能拿到可读文本
    e.dataTransfer.setData('text/plain', `block:${item.type}`);
    el.classList.add('dragging');
  });

  el.addEventListener('dragend', () => el.classList.remove('dragging'));

  return el;
}

function makeLibraryBlock(entry) {
  const el = document.createElement('div');
  el.className = `palette-block palette-lib${entry.nested ? ' nested' : ''}`;
  el.draggable = true;
  el.dataset.libName = entry.name;
  el.dataset.libGroup = entry.groupPath || '';
  el.setAttribute('role', 'button');
  el.tabIndex = 0;
  el.title = entry.nested
    ? `只取「${entry.name}」里的宏组「${entry.groupPath}」：${entry.count} 条动作\n拖进去＝插入真实动作`
    : `宏组「${entry.name}」：${entry.count} 条动作\n拖进去＝插入真实动作（不是引用）`;

  const icon = document.createElement('span');
  icon.className = 'pb-icon';
  icon.textContent = entry.nested ? '↳' : '🗂';

  const text = document.createElement('span');
  text.className = 'pb-text';
  const name = document.createElement('b');
  name.textContent = entry.label;
  const hint = document.createElement('small');
  hint.textContent = entry.nested
    ? `来自「${entry.name}」· ${entry.count} 块`
    : (entry.folder
      ? `${entry.count} 块 · 文件夹「${entry.folder}」`
      : `${entry.count} 块 · 未分类`);
  text.appendChild(name);
  text.appendChild(hint);

  const edit = document.createElement('button');
  edit.type = 'button';
  edit.className = 'pb-del pb-edit';
  edit.textContent = '✎';
  edit.title = entry.nested ? '只能在库里改整条宏组' : `改名 / 换文件夹：${entry.name}`;
  edit.hidden = Boolean(entry.nested);
  edit.addEventListener('click', async (e) => {
    e.stopPropagation();
    if (entry.nested) return;

    const payload = await openEditGroupDialog({
      name: entry.name,
      folder: entry.folder,
      folders: listGroupFolders(),
      taken: (name) => hasGroupInLibrary(name) && name !== entry.name,
    });
    if (!payload) return;

    const ret = updateGroupInLibrary(entry.name, { name: payload.name, folder: payload.folder });
    if (!ret.ok) {
      showToast(ret.reason === 'conflict' ? `库里已经有「${payload.name}」了` : '这条宏组已经不在了');
      renderBlockPalette();
      return;
    }
    renderBlockPalette();
    showToast(ret.folder
      ? `已更新宏组「${ret.name}」→ 文件夹「${ret.folder}」`
      : `已更新宏组「${ret.name}」→ 未分类`);
  });

  const del = document.createElement('button');
  del.type = 'button';
  del.className = 'pb-del';
  del.textContent = '×';
  del.title = entry.nested ? '只能在库里删整条宏组' : `从宏组库删掉「${entry.name}」`;
  del.hidden = Boolean(entry.nested);
  del.addEventListener('click', (e) => {
    e.stopPropagation();
    if (entry.nested) return;
    const ok = window.confirm(`从宏组库删掉「${entry.name}」？\n\n只删库里的副本，已经插进宏里的动作不受影响。`);
    if (!ok) return;
    if (!deleteGroupFromLibrary(entry.name)) {
      showToast('这条宏组已经不在库里了');
    } else {
      showToast(`已从宏组库删除「${entry.name}」`);
    }
    renderBlockPalette();
  });

  el.appendChild(icon);
  el.appendChild(text);
  el.appendChild(edit);
  el.appendChild(del);

  const insert = () => insertLibraryGroup(entry.name, entry.groupPath);

  el.addEventListener('click', insert);
  el.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      insert();
    }
  });
  el.addEventListener('dragstart', (e) => {
    e.dataTransfer.effectAllowed = 'copy';
    e.dataTransfer.setData(PALETTE_LIB_MIME, JSON.stringify({ name: entry.name, group: entry.groupPath || '' }));
    e.dataTransfer.setData('text/plain', `group:${entry.name}`);
    el.classList.add('dragging');
  });
  el.addEventListener('dragend', () => el.classList.remove('dragging'));

  return el;
}

/** 一个文件夹小节：标题行（可折叠 / 可删）+ 里面的宏组块 */
function makeFolderSection(folder, entries) {
  const wrap = document.createElement('div');
  wrap.className = `palette-folder${folder ? '' : ' palette-folder-unfiled'}`;

  const head = document.createElement('div');
  head.className = 'palette-folder-head';

  const collapsed = Boolean(folder) && state.groupFoldersCollapsed.has(folder);

  if (folder) {
    const toggle = document.createElement('button');
    toggle.type = 'button';
    toggle.className = 'palette-folder-toggle';
    toggle.textContent = collapsed ? '▸' : '▾';
    toggle.title = collapsed ? '展开这个文件夹' : '收起这个文件夹';
    toggle.addEventListener('click', (e) => {
      e.stopPropagation();
      if (collapsed) state.groupFoldersCollapsed.delete(folder);
      else state.groupFoldersCollapsed.add(folder);
      renderBlockPalette();
    });
    head.appendChild(toggle);
  }

  const nameEl = document.createElement('span');
  nameEl.className = 'palette-folder-name';
  nameEl.textContent = folder || '未分类';
  head.appendChild(nameEl);

  const countEl = document.createElement('span');
  countEl.className = 'palette-folder-count';
  countEl.textContent = String(entries.filter((entry) => !entry.nested).length);
  head.appendChild(countEl);

  if (folder) {
    const del = document.createElement('button');
    del.type = 'button';
    del.className = 'palette-folder-del';
    del.textContent = '×';
    del.title = '删掉这个文件夹（里面的宏组退回「未分类」，不会被删）';
    del.addEventListener('click', (e) => {
      e.stopPropagation();
      const ok = window.confirm(`删掉文件夹「${folder}」？\n\n里面的宏组不会被删，会退回「未分类」。`);
      if (!ok) return;
      const ret = deleteGroupFolder(folder);
      state.groupFoldersCollapsed.delete(folder);
      renderBlockPalette();
      if (!ret.ok) {
        showToast(`文件夹「${folder}」已经没了`);
        return;
      }
      showToast(ret.moved
        ? `已删掉文件夹「${folder}」，${ret.moved} 个宏组退回未分类`
        : `已删掉文件夹「${folder}」`);
    });
    head.appendChild(del);
  }

  wrap.appendChild(head);

  if (collapsed) return wrap;

  if (!entries.length) {
    const empty = document.createElement('div');
    empty.className = 'palette-folder-empty';
    empty.textContent = folder ? '空文件夹：把宏组编辑到这儿，或者删掉它。' : '还没有宏组。';
    wrap.appendChild(empty);
    return wrap;
  }

  entries.forEach((entry) => wrap.appendChild(makeLibraryBlock(entry)));
  return wrap;
}

/** 重画整个左栏块库（静态块 + 我的宏组）；宏组库变了就调它 */
export function renderBlockPalette() {
  const host = q('#blockPalette');
  if (!host) return;

  host.textContent = '';

  let staticCount = 0;
  BLOCK_CATALOG.forEach((cat) => {
    const title = document.createElement('div');
    title.className = 'palette-cat';
    title.textContent = cat.group;
    host.appendChild(title);

    cat.items.forEach((item) => {
      staticCount += 1;
      host.appendChild(makePaletteBlock(item));
    });
  });

  const entries = groupLibraryEntries();
  const folders = listGroupFolders();

  const libTitle = document.createElement('div');
  libTitle.className = 'palette-cat palette-cat-lib';

  const libLabel = document.createElement('span');
  libLabel.className = 'palette-cat-label';
  libLabel.textContent = '我的宏组 · 直接拖用';

  const libTools = document.createElement('span');
  libTools.className = 'palette-cat-tools';

  const saveGroupBtn = document.createElement('button');
  saveGroupBtn.type = 'button';
  saveGroupBtn.className = 'palette-mini-btn';
  saveGroupBtn.textContent = '存组';
  saveGroupBtn.title = '把当前选中的宏组（没选就取第一个）存进库：可以起名字、选文件夹';
  saveGroupBtn.addEventListener('click', (e) => {
    e.stopPropagation();
    if (typeof state.saveGroupFromPalette === 'function') {
      state.saveGroupFromPalette();
    } else {
      showToast('页面还没准备好，稍等一下再点');
    }
  });
  libTools.appendChild(saveGroupBtn);

  const addFolder = document.createElement('button');
  addFolder.type = 'button';
  addFolder.className = 'palette-mini-btn';
  addFolder.textContent = '＋ 文件夹';
  addFolder.title = '新建一个文件夹，用来给宏组分类';
  addFolder.addEventListener('click', async (e) => {
    e.stopPropagation();
    const payload = await openNewFolderDialog({ folders: listGroupFolders() });
    if (!payload) return;
    const ret = createGroupFolder(payload.folder);
    renderBlockPalette();
    showToast(ret.created ? `已新建文件夹「${ret.folder}」` : `文件夹「${ret.folder}」已经有了`);
  });
  libTools.appendChild(addFolder);

  libTitle.append(libLabel, libTools);
  host.appendChild(libTitle);

  if (!entries.length && !folders.length) {
    const empty = document.createElement('div');
    empty.className = 'palette-empty';
    empty.textContent = '库是空的。在序列里选中一个宏组，点它头上的「⤓ 存组」，起个名字就出现在这儿。';
    host.appendChild(empty);
  } else {
    // 按文件夹分组：登记的文件夹在前（按名字），未分类垫底
    const byFolder = new Map();
    folders.forEach((folder) => byFolder.set(folder, []));
    byFolder.set('', []);

    entries.forEach((entry) => {
      const key = entry.folder || '';
      if (!byFolder.has(key)) byFolder.set(key, []);
      byFolder.get(key).push(entry);
    });

    folders.forEach((folder) => {
      host.appendChild(makeFolderSection(folder, byFolder.get(folder) || []));
    });

    const unfiled = byFolder.get('') || [];
    if (unfiled.length || !folders.length) {
      host.appendChild(makeFolderSection('', unfiled));
    }
  }

  const count = q('#paletteCount');
  if (count) count.textContent = `${staticCount + entries.length} 块`;
}

/** 从 dataTransfer 里认出「块库拖来的东西」；不是块库拖的就返回 null */
export function readPaletteDrag(dataTransfer) {
  if (!dataTransfer) return null;

  const libRaw = dataTransfer.getData(PALETTE_LIB_MIME);
  if (libRaw) {
    try {
      const parsed = JSON.parse(libRaw);
      if (parsed && parsed.name) return { kind: 'library', name: String(parsed.name), group: String(parsed.group || '') };
    } catch (err) {
      /* 掉到下面的 block 分支 */
    }
  }

  const type = dataTransfer.getData(PALETTE_BLOCK_MIME);
  if (type && getPaletteBlock(type)) return { kind: 'block', type };

  return null;
}

/** 把块库拖来的东西落到某个 target 上；成功返回 true */
export function dropPalettePayload(payload, target) {
  if (!payload) return false;
  if (payload.kind === 'library') {
    insertLibraryGroup(payload.name, payload.group, target);
    return true;
  }
  if (payload.kind === 'block') {
    if (payload.type === 'group') {
      void insertNamedGroupBlock(target);
      return true;
    }
    insertPaletteBlock(payload.type, target);
    return true;
  }
  return false;
}
