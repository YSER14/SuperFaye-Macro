// -*- coding: utf-8 -*-
/**
 * 宏组库（照 XMBC 拼宏编辑器的「我的宏组」搬过来，再补上文件夹与命名）。
 *
 * 存的是可复用的宏组：`['group', '组名', [子动作…]]` 的子动作数组。
 * 用 localStorage 保存，键 `macrodriver.group-lib`，v2 结构：
 *
 *     {
 *       version: 2,
 *       folders: ["开场", "连招/BOSS"],          // 文件夹（扁平字符串，可带 / 做分组显示）
 *       groups: [
 *         { id, name, folder, rows: [...], savedAt: 1699999999999 }
 *       ]
 *     }
 *
 * 兼容 v1（老版本是 `{ 组名: { name, rows, savedAt } }` 的平铺对象），
 * 读到就自动迁移：所有老组进「未分类」，写回时升级成 v2。
 *
 * 名字在库里全局唯一（左栏、拖拽、插入都按名字索引），重名保存＝覆盖。
 * 拖/点左栏条目 = 把**真实动作**插进序列（包成一个宏组），不是引用框；
 * 存盘时宏组照样摊平，所以库里怎么攒都不影响生成的宏文件格式。
 */
import { cloneAction, countActionsDeep, findGroupPaths, selectGroupBody } from './action-model.js';

export const GROUP_LIB_STORAGE_KEY = 'macrodriver.group-lib';
export const UNFILED_FOLDER = '';
const SCHEMA_VERSION = 2;

let cache = null;
let diskBaseline = null;
let pendingWrite = Promise.resolve();

function newId() {
  return `g_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 8)}`;
}

/** 文件夹名归一：去空白、去首尾斜杠、压掉重复斜杠 */
export function normalizeFolder(value) {
  return String(value ?? '')
    .trim()
    .replace(/\\/g, '/')
    .replace(/\/{2,}/g, '/')
    .replace(/^\/+|\/+$/g, '');
}

function normalizeName(value) {
  return String(value ?? '').trim();
}

function blankLibrary() {
  return { version: SCHEMA_VERSION, folders: [], groups: [] };
}

function normalizeRows(rows) {
  return (Array.isArray(rows) ? rows : []).map((row) => cloneAction(row));
}

/** v1 / 脏数据 → v2；认不出就返回空库 */
function migrate(raw) {
  const lib = blankLibrary();
  if (!raw || typeof raw !== 'object') return lib;

  // 已经是 v2
  if (raw.version === SCHEMA_VERSION && Array.isArray(raw.groups)) {
    lib.folders = [...new Set((Array.isArray(raw.folders) ? raw.folders : [])
      .map(normalizeFolder)
      .filter(Boolean))];
    const seen = new Set();
    (raw.groups || []).forEach((g) => {
      if (!g || typeof g !== 'object') return;
      const name = normalizeName(g.name);
      if (!name || seen.has(name)) return;
      seen.add(name);
      lib.groups.push({
        id: String(g.id || newId()),
        name,
        folder: normalizeFolder(g.folder),
        rows: normalizeRows(g.rows),
        savedAt: Number(g.savedAt) || Date.now(),
      });
    });
    return lib;
  }

  // v1：{ 组名: { name, rows, savedAt } }
  Object.entries(raw).forEach(([key, value]) => {
    if (!value || typeof value !== 'object' || Array.isArray(value)) return;
    const name = normalizeName(value.name || key);
    if (!name) return;
    lib.groups.push({
      id: newId(),
      name,
      folder: normalizeFolder(value.folder),
      rows: normalizeRows(value.rows),
      savedAt: Number(value.savedAt) || Date.now(),
    });
  });
  return lib;
}

function readRaw() {
  try {
    const text = window.localStorage.getItem(GROUP_LIB_STORAGE_KEY);
    if (!text) return blankLibrary();
    return migrate(JSON.parse(text));
  } catch (err) {
    return blankLibrary();
  }
}

function writeRaw(lib) {
  cache = lib;
  let persisted = true;
  try {
    window.localStorage.setItem(GROUP_LIB_STORAGE_KEY, JSON.stringify(lib));
  } catch (err) {
    // 隐私模式 / 配额满：内存里仍然可用
    persisted = false;
  }
  syncToDisk(lib);
  return persisted;
}

/** 只更新内存 + localStorage 缓存，不回写磁盘（载入磁盘数据时用） */
function writeLocalCache(lib) {
  cache = lib;
  try {
    window.localStorage.setItem(GROUP_LIB_STORAGE_KEY, JSON.stringify(lib));
  } catch (err) {
    /* 缓存写不进也不影响这次会话 */
  }
}

/**
 * 把整库写到后端 JSON。
 * localStorage 只是缓存：pywebview 以前是 private_mode，退出就清空，
 * 真正的存档在 `webui_group_lib.json`（bridge.save_group_lib）。
 */
function syncToDisk(lib) {
  const api = window.pywebview && window.pywebview.api;
  if (!api || typeof api.save_group_lib !== 'function') return;
  const next = structuredClone(lib);
  pendingWrite = pendingWrite.then(async () => {
    const result = await api.save_group_lib(next, diskBaseline);
    if (result?.ok) diskBaseline = next;
  }).catch(() => {});
}

/** 两边合并：以磁盘为底，本地缓存里 savedAt 更新的组覆盖它 */
function mergeLibraries(remote, local) {
  const out = blankLibrary();
  out.folders = [...new Set([
    ...(Array.isArray(remote.folders) ? remote.folders : []),
    ...(Array.isArray(local.folders) ? local.folders : []),
  ])].sort((a, b) => a.localeCompare(b, 'zh-Hans-CN'));

  const byName = new Map();
  (remote.groups || []).forEach((group) => byName.set(group.name, group));
  (local.groups || []).forEach((group) => {
    const existing = byName.get(group.name);
    if (!existing || (group.savedAt || 0) > (existing.savedAt || 0)) {
      byName.set(group.name, group);
    }
  });
  out.groups = [...byName.values()];
  return out;
}

/**
 * 启动时调用：把磁盘上的宏组库读进来。
 * 磁盘还是空的（第一次升级 / 老用户）就把本地缓存补写过去；
 * 磁盘有数据就以它为准，和缓存按 savedAt 合并。
 * @returns {Promise<{source:string, groups?:number}>}
 */
export async function hydrateGroupLibrary() {
  const api = window.pywebview && window.pywebview.api;
  if (!api || typeof api.load_group_lib !== 'function') {
    return { source: 'no-api' };
  }

  let remote = null;
  let needsMigration = false;
  try {
    const result = await api.load_group_lib();
    if (result && result.ok && result.lib) {
      remote = migrate(result.lib);
      needsMigration = result.lib.version !== SCHEMA_VERSION || !Array.isArray(result.lib.groups);
    }
  } catch (err) {
    return { source: 'error' };
  }

  const local = loadGroupLibrary();

  if (!remote) {
    // 磁盘没东西：把当前缓存（可能是升级前攒的）写成第一份存档
    if (local.groups.length || local.folders.length) syncToDisk(local);
    return { source: 'missing', groups: local.groups.length };
  }

  diskBaseline = structuredClone(remote);
  const merged = mergeLibraries(remote, local);
  const diskNeedsUpdate = needsMigration || merged.groups.length !== remote.groups.length
    || merged.folders.length !== remote.folders.length
    || local.groups.some((group) => {
      const kept = remote.groups.find((item) => item.name === group.name);
      return kept && (group.savedAt || 0) > (kept.savedAt || 0);
    });

  writeLocalCache(merged);
  if (diskNeedsUpdate) syncToDisk(merged);
  return { source: 'disk', groups: merged.groups.length, changed: diskNeedsUpdate };
}

/** 读整个库（内存缓存，写操作会同步刷新） */
export function loadGroupLibrary() {
  if (!cache) cache = readRaw();
  return cache;
}

/** 丢掉缓存，下次重新从 localStorage 读（另一个窗口改过库时用） */
export function invalidateGroupLibrary() {
  cache = null;
}

export async function reloadGroupLibrary() {
  const existing = pendingWrite;
  await existing;
  const result = await window.pywebview.api.load_group_lib();
  if (existing !== pendingWrite) return reloadGroupLibrary();
  if (result.ok && result.lib) {
    const remote = migrate(result.lib);
    diskBaseline = structuredClone(remote);
    writeLocalCache(remote);
  }
}

/* ───────────────────────────── 查询 ───────────────────────────── */

export function groupNames() {
  return loadGroupLibrary().groups.map((g) => g.name);
}

export function hasGroupInLibrary(name) {
  const clean = normalizeName(name);
  return loadGroupLibrary().groups.some((g) => g.name === clean);
}

export function findGroup(name) {
  const clean = normalizeName(name);
  return loadGroupLibrary().groups.find((g) => g.name === clean) || null;
}

/** 文件夹清单：登记的 + 组里实际用到的，排序后返回（未分类不算文件夹） */
export function listGroupFolders() {
  const lib = loadGroupLibrary();
  const set = new Set(lib.folders.map(normalizeFolder).filter(Boolean));
  lib.groups.forEach((g) => {
    const folder = normalizeFolder(g.folder);
    if (folder) set.add(folder);
  });
  return [...set].sort((a, b) => a.localeCompare(b, 'zh-Hans-CN'));
}

/* ───────────────────────────── 文件夹 ───────────────────────────── */

export function createGroupFolder(name) {
  const folder = normalizeFolder(name);
  if (!folder) return { ok: false, reason: 'empty' };

  const lib = loadGroupLibrary();
  if (!lib.folders.includes(folder)) {
    lib.folders.push(folder);
    lib.folders.sort((a, b) => a.localeCompare(b, 'zh-Hans-CN'));
    writeRaw(lib);
    return { ok: true, folder, created: true, persisted: true };
  }
  return { ok: true, folder, created: false };
}

/** 删文件夹：里面的宏组不删，退回「未分类」 */
export function deleteGroupFolder(name) {
  const folder = normalizeFolder(name);
  const lib = loadGroupLibrary();
  const existed = lib.folders.includes(folder);
  lib.folders = lib.folders.filter((item) => item !== folder);
  let moved = 0;
  lib.groups.forEach((g) => {
    if (normalizeFolder(g.folder) === folder) {
      g.folder = UNFILED_FOLDER;
      moved += 1;
    }
  });
  if (existed || moved) {
    writeRaw(lib);
    return { ok: true, moved };
  }
  return { ok: false, moved: 0 };
}

/* ───────────────────────────── 增删改 ───────────────────────────── */

/**
 * 把一个宏组存进库。
 * @param {string} name 组名（库里唯一）
 * @param {Array} rows 组里那一串动作
 * @param {{folder?:string, overwrite?:boolean}} [options]
 *        folder 不给就沿用库里原位置（新组＝未分类）；overwrite=false 时重名返回 conflict
 * @returns {{ok:boolean, name:string, updated:boolean, persisted:boolean, folder:string, conflict?:boolean}}
 */
export function saveGroupToLibrary(name, rows, options = {}) {
  const clean = normalizeName(name) || '未命名宏组';
  const lib = loadGroupLibrary();
  const existing = lib.groups.find((g) => g.name === clean);

  if (existing && options.overwrite === false) {
    return {
      ok: false,
      conflict: true,
      name: clean,
      updated: true,
      persisted: false,
      folder: existing.folder,
    };
  }

  const folderWanted = Object.prototype.hasOwnProperty.call(options, 'folder')
    ? normalizeFolder(options.folder)
    : (existing ? existing.folder : UNFILED_FOLDER);

  if (existing) {
    existing.rows = normalizeRows(rows);
    existing.folder = folderWanted;
    existing.savedAt = Date.now();
  } else {
    lib.groups.push({
      id: newId(),
      name: clean,
      folder: folderWanted,
      rows: normalizeRows(rows),
      savedAt: Date.now(),
    });
  }

  if (folderWanted && !lib.folders.includes(folderWanted)) {
    lib.folders.push(folderWanted);
    lib.folders.sort((a, b) => a.localeCompare(b, 'zh-Hans-CN'));
  }

  const persisted = writeRaw(lib);
  return { ok: true, name: clean, updated: Boolean(existing), persisted, folder: folderWanted };
}

/** 改名（保持原名唯一；新名字已被别的组占了就 conflict） */
export function renameGroupInLibrary(oldName, newName) {
  const from = normalizeName(oldName);
  const to = normalizeName(newName);
  if (!from || !to) return { ok: false, reason: 'empty' };
  if (from === to) return { ok: true, name: to, unchanged: true };

  const lib = loadGroupLibrary();
  const group = lib.groups.find((g) => g.name === from);
  if (!group) return { ok: false, reason: 'missing' };
  if (lib.groups.some((g) => g.name === to)) return { ok: false, reason: 'conflict', name: to };

  group.name = to;
  group.savedAt = Date.now();
  const persisted = writeRaw(lib);
  return { ok: true, name: to, persisted };
}

export function moveGroupToFolder(name, folder) {
  const clean = normalizeName(name);
  const lib = loadGroupLibrary();
  const group = lib.groups.find((g) => g.name === clean);
  if (!group) return { ok: false, reason: 'missing' };

  const target = normalizeFolder(folder);
  group.folder = target;
  if (target && !lib.folders.includes(target)) {
    lib.folders.push(target);
    lib.folders.sort((a, b) => a.localeCompare(b, 'zh-Hans-CN'));
  }
  const persisted = writeRaw(lib);
  return { ok: true, name: clean, folder: target, persisted };
}

/** 改名 + 换文件夹一次做完（编辑对话框用） */
export function updateGroupInLibrary(oldName, options = {}) {
  const from = normalizeName(oldName);
  const to = normalizeName(options.name) || from;
  const lib = loadGroupLibrary();
  const group = lib.groups.find((g) => g.name === from);
  if (!group) return { ok: false, reason: 'missing' };

  if (to !== from && lib.groups.some((g) => g.name === to)) {
    return { ok: false, reason: 'conflict', name: to };
  }

  const target = Object.prototype.hasOwnProperty.call(options, 'folder')
    ? normalizeFolder(options.folder)
    : group.folder;

  group.name = to;
  group.folder = target;
  group.savedAt = Date.now();
  if (target && !lib.folders.includes(target)) {
    lib.folders.push(target);
    lib.folders.sort((a, b) => a.localeCompare(b, 'zh-Hans-CN'));
  }
  const persisted = writeRaw(lib);
  return { ok: true, name: to, folder: target, persisted };
}

export function deleteGroupFromLibrary(name) {
  const clean = normalizeName(name);
  const lib = loadGroupLibrary();
  const before = lib.groups.length;
  lib.groups = lib.groups.filter((g) => g.name !== clean);
  if (lib.groups.length === before) return false;
  writeRaw(lib);
  return true;
}

/* ───────────────────────────── 左栏数据 ───────────────────────────── */

function folderRank(folder) {
  // 未分类排最后：有文件夹的按名字，空文件夹垫底
  return folder ? 1 : 2;
}

/**
 * 左栏要列的东西：每条宏组，外加它内部嵌套的每个组（照 XMBC 编辑器那套「组路径」）。
 * 结果按文件夹排好序，调用方按 folder 分组渲染即可。
 * @returns {Array<{name:string, folder:string, groupPath:string, label:string, count:number, nested:boolean}>}
 */
export function groupLibraryEntries() {
  const out = [];
  const lib = loadGroupLibrary();
  const groups = [...lib.groups].sort((a, b) => {
    const rank = folderRank(normalizeFolder(a.folder)) - folderRank(normalizeFolder(b.folder));
    if (rank !== 0) return rank;
    const folderCmp = normalizeFolder(a.folder).localeCompare(normalizeFolder(b.folder), 'zh-Hans-CN');
    if (folderCmp !== 0) return folderCmp;
    return a.name.localeCompare(b.name, 'zh-Hans-CN');
  });

  groups.forEach((group) => {
    const rows = Array.isArray(group.rows) ? group.rows : [];
    const folder = normalizeFolder(group.folder);
    out.push({
      name: group.name,
      folder,
      groupPath: '',
      label: group.name,
      count: countActionsDeep(rows),
      nested: false,
    });

    findGroupPaths(rows).forEach((groupPath) => {
      const body = selectGroupBody(rows, groupPath);
      out.push({
        name: group.name,
        folder,
        groupPath,
        label: groupPath.split('/').pop(),
        count: countActionsDeep(body),
        nested: true,
      });
    });
  });

  return out;
}

/** 把库里某条（或其中某个组）变成可插入的宏组行；源没了就返回 null */
export function rowsFromLibrary(name, groupPath = '') {
  const group = findGroup(name);
  if (!group) return null;

  const rows = Array.isArray(group.rows) ? group.rows : [];
  const body = groupPath ? selectGroupBody(rows, groupPath) : rows;
  const label = groupPath ? String(groupPath).split('/').pop() : group.name;
  return ['group', label, cloneAction(body)];
}
