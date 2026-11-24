import { state } from './core/state.js';
import { q, qa, showToast } from './core/dom.js';
import { apiCall } from './core/api.js';
import { refreshListenerButtonState, refreshSnapshot } from './dashboard.js';
import { renderMacroTreeList } from './macro-tree.js';
import { openImportMacroDialog } from './macro-import.js';

// 触发方式的中文显示（后端存的仍是 normal / pause_resume 这类英文原值）
const PRESS_MODE_LABELS = {
  normal: '普通（按一次跑一次）',
  pause_resume: '暂停 / 继续切换',
  hold_pause: '按住暂停',
  stop_restart: '停止并重启',
  parallel_trigger: '并发触发',
  release_stop: '抬起即停止',
  release_pause: '抬起即暂停',
};

// 触发键的中文显示（绑定、触发仍用 v / mouse_x1 这些原始键名）
// 后端实际会写出 caps_lock / num_lock 这种带下划线的名字，两个写法都收
const TRIGGER_KEY_LABELS = {
  mouse_x1: '鼠标侧键 1',
  mouse_x2: '鼠标侧键 2',
  mouse_left: '鼠标左键',
  mouse_right: '鼠标右键',
  mouse_middle: '鼠标中键',
  mouse_wheel_up: '滚轮向上',
  mouse_wheel_down: '滚轮向下',
  esc: 'Esc 键',
  space: '空格键',
  enter: '回车键',
  tab: 'Tab 键',
  shift: 'Shift 键',
  ctrl: 'Ctrl 键',
  alt: 'Alt 键',
  backspace: '退格键',
  delete: 'Delete 键',
  insert: 'Insert 键',
  home: 'Home 键',
  end: 'End 键',
  page_up: 'PageUp 键',
  page_down: 'PageDown 键',
  up: '↑ 上方向键',
  down: '↓ 下方向键',
  left: '← 左方向键',
  right: '→ 右方向键',
  capslock: 'CapsLock 键',
  caps_lock: 'CapsLock 键',
  numlock: 'NumLock 键',
  num_lock: 'NumLock 键',
};

const VALID_PRESS_MODES = Object.keys(PRESS_MODE_LABELS);

// 每种触发模式的一句话解释：选到哪个就在下拉框下面提示哪种行为
const MODE_HINTS = {
  normal: '按一次跑一遍就结束；抬起不做任何事。最常用。',
  pause_resume: '第一次按下启动；运行时再按暂停，暂停后再按继续。',
  hold_pause: '第一次松开时开始执行；运行期间按住暂停，松开继续。',
  stop_restart: '每次按下都先终止旧实例、等它退出，再从第一帧重新开始。',
  parallel_trigger: '每次按下都新开一份实例，多份同时跑，会越按越多。',
  release_stop: '按下开始、抬起立刻终止。只有按住不放才跑得下去，轻点一下几乎等于没跑。',
  release_pause: '按下开始、抬起暂停并保留进度，再按从断点继续。',
};

function syncBindModeHint() {
  const hint = q('#bindModeHint');
  if (!hint) return;
  const select = q('#bindMode');
  hint.textContent = MODE_HINTS[select ? select.value : 'normal'] || '';
}

function formatTriggerLabel(trigger) {
  const raw = String(trigger ?? '').trim();
  if (!raw) return '（未设置）';
  return raw
    .split('+')
    .map((part) => {
      const name = part.trim();
      if (!name) return '';
      const lower = name.toLowerCase();
      if (TRIGGER_KEY_LABELS[lower]) return TRIGGER_KEY_LABELS[lower];
      if (/^f([1-9]|1[0-2])$/.test(lower)) return `${lower.toUpperCase()} 键`;
      if (name.length === 1) return `「${name.toUpperCase()}」键`;
      return name;
    })
    .filter(Boolean)
    .join(' + ');
}

function parseNumber(text, fallback = 0) {
  const n = Number(text);
  return Number.isFinite(n) ? n : fallback;
}

function syncRuntimeProfileUI() {
  q('#macroSpeedFactor').value = String(state.runtimeProfile.speed_factor ?? 1);
  q('#macroRandomMin').value = String(state.runtimeProfile.random_min_ms ?? 0);
  q('#macroRandomMax').value = String(state.runtimeProfile.random_max_ms ?? 0);
  q('#btnMacroRandomToggle').textContent = `随机延迟：${state.runtimeProfile.random_enabled ? '开' : '关'}`;
  q('#btnLowCpuModeToggle').textContent = `CPU 低占用：${state.runtimeProfile.low_cpu_enabled ? '开' : '关'}`;
  const timePeriodBtn = q('#btnTimePeriodToggle');
  timePeriodBtn.textContent = `计时器分辨率（1ms）：${state.runtimeProfile.low_cpu_time_period ? '开' : '关'}`;
  timePeriodBtn.style.display = state.runtimeProfile.low_cpu_enabled ? '' : 'none';
}

export async function refreshMacroRuntimeProfile() {
  const data = await apiCall('get_macro_runtime_profile');
  state.runtimeProfile = {
    speed_factor: data.speed_factor,
    random_enabled: data.random_enabled,
    random_min_ms: data.random_min_ms,
    random_max_ms: data.random_max_ms,
    low_cpu_enabled: Boolean(data.low_cpu_enabled),
    low_cpu_time_period: Boolean(data.low_cpu_time_period),
  };
  syncRuntimeProfileUI();
}

async function applyMacroRuntimeProfile() {
  const payload = {
    speed_factor: parseNumber(q('#macroSpeedFactor').value, 1),
    random_enabled: Boolean(state.runtimeProfile.random_enabled),
    random_min_ms: parseNumber(q('#macroRandomMin').value, 0),
    random_max_ms: parseNumber(q('#macroRandomMax').value, 0),
    low_cpu_enabled: Boolean(state.runtimeProfile.low_cpu_enabled),
    low_cpu_time_period: Boolean(state.runtimeProfile.low_cpu_time_period),
  };
  const ret = await apiCall('update_macro_runtime_profile', payload);
  state.runtimeProfile = {
    speed_factor: ret.speed_factor,
    random_enabled: ret.random_enabled,
    random_min_ms: ret.random_min_ms,
    random_max_ms: ret.random_max_ms,
    low_cpu_enabled: Boolean(ret.low_cpu_enabled),
    low_cpu_time_period: Boolean(ret.low_cpu_time_period),
  };
  syncRuntimeProfileUI();
  showToast('执行参数已应用');
}

export function renderMacroList() {
  const list = q('#macroList');
  renderMacroTreeList({
    container: list,
    tree: state.macroTree,
    viewId: 'macro',
    selectedPath: state.selectedMacro,
    openedPath: null,
    onSelect: (path) => {
      state.selectedMacro = path;
      q('#selectedMacroPath').value = path;
      q('#simplifyInput').value = path;
      renderMacroList();
      if (state.onSelectMacroForEditor) {
        state.onSelectMacroForEditor(path).catch((err) => showToast(err.message));
      }
    },
  });
}

export async function refreshMacros() {
  const keyword = q('#macroFilter').value.trim();
  try {
    const data = await apiCall('list_macro_tree', keyword);
    state.macros = data.items || [];
    state.macroTree = data.tree || { name: '宏', path: '宏', folders: [], files: [] };
  } catch (err) {
    state.macros = [];
    state.macroTree = { name: '宏', path: '宏', folders: [], files: [] };
  }

  if (state.selectedMacro && !state.macros.some((m) => m.path === state.selectedMacro)) {
    state.selectedMacro = null;
    q('#selectedMacroPath').value = '';
  }

  renderMacroList();

  const { renderEditorMacroBrowser } = await import('./editor/index.js');
  const { renderRecordMacroBrowser } = await import('./recorder.js');
  renderEditorMacroBrowser();
  renderRecordMacroBrowser();
}

// 最近一次拿到的绑定行：点行回填表单时要能就地重绘高亮，不用等 900ms 轮询
const bindingRows = new Map();
let bindingRefreshId = 0;
let appliedBindingRefreshId = 0;

function bindingMacroPaths(row) {
  if (Array.isArray(row.macro_paths) && row.macro_paths.length) return row.macro_paths;
  return row.macro_path ? [row.macro_path] : [];
}

function bindingStatusText(row) {
  const paused = Number(row.paused_count || 0);
  const running = Number(row.live_count || 0) - paused;
  if (running > 0 && paused > 0) return `运行中 ${running}｜已暂停 ${paused}`;
  if (paused > 0) return `已暂停 ${paused}`;
  if (running > 0) return `运行中 ${running}`;
  if (Number(row.stopping_count || 0) > 0) return '停止中';
  return '空闲';
}

function updateBindingSelection() {
  for (const [trigger, { element }] of bindingRows) {
    element.classList.toggle('selected', trigger === state.selectedBindingTrigger);
  }
}

async function refreshSchemeTabsQuietly() {
  try {
    const mod = await import('./macro-schemes.js');
    await mod.refreshSchemes();
  } catch (err) {
    console.warn('刷新方案标签失败：', err);
  }
}

/** 取消「现有绑定」里的选中态（切换方案后调用）。 */
export function clearBindingSelection() {
  state.selectedBindingTrigger = null;
  updateBindingSelection();
}

/**
 * 点「现有绑定」某一行 → 把这条绑定的宏 / 重复次数 / 触发模式回填到上面的表单。
 * 回填只是"载入"，要点「绑定当前宏」才会真正写回后端。
 */
function applyBindingToForm(row) {
  const macroPaths = bindingMacroPaths(row);
  const firstMacro = macroPaths[0] || '';

  state.selectedMacro = firstMacro || null;
  const pathInput = q('#selectedMacroPath');
  if (pathInput) pathInput.value = firstMacro;

  const triggerInput = q('#bindTrigger');
  if (triggerInput) triggerInput.value = row.trigger || '';
  const repeatInput = q('#bindRepeat');
  if (repeatInput) repeatInput.value = String(row.repeat || 1);
  const modeSelect = q('#bindMode');
  if (modeSelect) {
    const mode = String(row.running_press_mode || 'normal');
    modeSelect.value = VALID_PRESS_MODES.includes(mode) ? mode : 'normal';
  }

  state.selectedBindingTrigger = row.trigger || null;
  syncBindModeHint();
  renderMacroList();
  updateBindingSelection();

  const extra = macroPaths.length > 1 ? `（这个键现在绑了 ${macroPaths.length} 个宏，重新绑定只保留当前宏）` : '';
  showToast(`已载入绑定：${formatTriggerLabel(row.trigger)}${extra}`);
}

function createBindingRow(initialRow) {
  let row = initialRow;
  const trigger = row.trigger;
  let saving = false;
  const li = document.createElement('li');
  li.classList.add('binding-row');
  li.dataset.trigger = trigger;
  li.title = '点一下把这条绑定（宏 / 重复次数 / 触发模式）载入上面的表单';

  const left = document.createElement('div');
  left.className = 'binding-left';

  const actions = document.createElement('div');
  actions.className = 'inline-actions binding-actions';

  // 触发模式：改完立刻保存到这条绑定，不用重新走一遍「选宏 + 绑定」
  const modeSelect = document.createElement('select');
  modeSelect.className = 'binding-mode-select';
  modeSelect.setAttribute('aria-label', `${formatTriggerLabel(trigger)}的触发模式`);
  for (const [value, label] of Object.entries(PRESS_MODE_LABELS)) {
    const opt = document.createElement('option');
    opt.value = value;
    opt.textContent = label;
    modeSelect.appendChild(opt);
  }
  modeSelect.addEventListener('click', (e) => e.stopPropagation());
  modeSelect.addEventListener('change', async (e) => {
    e.stopPropagation();
    const nextMode = modeSelect.value;
    saving = true;
    modeSelect.disabled = true;
    try {
      const ret = await apiCall('update_binding_runtime', trigger, Number(row.repeat || 1), nextMode);
      const mode = ret.running_press_mode;
      appliedBindingRefreshId = ++bindingRefreshId;
      update({ ...row, running_press_mode: mode });
      showToast(`已保存：${formatTriggerLabel(trigger)} → ${PRESS_MODE_LABELS[mode]}`);
      if (li.isConnected && state.selectedBindingTrigger === trigger) {
        const formSelect = q('#bindMode');
        if (formSelect) formSelect.value = mode;
        syncBindModeHint();
      }
    } catch (err) {
      showToast(err.message);
    } finally {
      saving = false;
      modeSelect.disabled = false;
      update(row);
      await refreshBindings().catch(() => {});
      refreshSchemeTabsQuietly();
    }
  });

  const btnRun = document.createElement('button');
  btnRun.textContent = '试跑';
  btnRun.title = '立刻试跑一遍宏';
  btnRun.addEventListener('click', async (e) => {
    e.stopPropagation();
    await runBindingOnce(trigger);
  });

  const btnUnbind = document.createElement('button');
  btnUnbind.className = 'danger';
  btnUnbind.textContent = '解绑';
  btnUnbind.addEventListener('click', async (e) => {
    e.stopPropagation();
    await apiCall('unbind_macro', trigger);
    showToast(`已解绑：${formatTriggerLabel(trigger)}`);
    if (state.selectedBindingTrigger === trigger) state.selectedBindingTrigger = null;
    await refreshBindings();
    refreshSchemeTabsQuietly();
  });

  actions.appendChild(modeSelect);
  actions.appendChild(btnRun);
  actions.appendChild(btnUnbind);

  li.addEventListener('click', () => applyBindingToForm(row));

  li.appendChild(left);
  li.appendChild(actions);

  function update(nextRow) {
    row = nextRow;
    const mode = VALID_PRESS_MODES.includes(row.running_press_mode) ? row.running_press_mode : 'normal';
    const macroDisplay = bindingMacroPaths(row).join(' + ') || '（未指定宏）';
    const label = `触发键 ${formatTriggerLabel(trigger)} → ${macroDisplay}｜重复 ${row.repeat} 次｜${PRESS_MODE_LABELS[mode]}｜${bindingStatusText(row)}`;
    if (left.textContent !== label) left.textContent = label;
    li.classList.toggle('running', Number(row.live_count || 0) > Number(row.paused_count || 0));
    if (!saving && modeSelect.value !== mode) modeSelect.value = mode;
    modeSelect.title = MODE_HINTS[mode];
  }

  return { element: li, update };
}

export async function refreshBindings() {
  const requestId = ++bindingRefreshId;
  const data = await apiCall('list_bindings');
  if (requestId < appliedBindingRefreshId) return;
  appliedBindingRefreshId = requestId;
  const items = data.items || [];
  const triggers = new Set(items.map((row) => row.trigger));
  const list = q('#bindingList');
  for (const [trigger, view] of bindingRows) {
    if (!triggers.has(trigger)) {
      view.element.remove();
      bindingRows.delete(trigger);
    }
  }
  if (!triggers.has(state.selectedBindingTrigger)) state.selectedBindingTrigger = null;

  if (!items.length) {
    if (!list.firstElementChild) {
      const empty = document.createElement('li');
      empty.className = 'binding-empty';
      empty.textContent = '当前方案还没有绑定：在上面选好宏和触发键，点「绑定当前宏」';
      list.appendChild(empty);
    }
    return;
  }

  list.querySelector('.binding-empty')?.remove();
  let nextElement = list.firstElementChild;
  for (const row of items) {
    let view = bindingRows.get(row.trigger);
    if (!view) {
      view = createBindingRow(row);
      bindingRows.set(row.trigger, view);
    }
    view.update(row);
    // 轮询只更新状态。原地保留下拉框，避免原生菜单被关闭。
    if (view.element !== nextElement) list.insertBefore(view.element, nextElement);
    nextElement = view.element.nextElementSibling;
  }
  updateBindingSelection();
}

async function runBindingOnce(trigger) {
  try {
    await apiCall('trigger_macro', trigger);
    showToast(`已开始试跑：${formatTriggerLabel(trigger)}`);
    await refreshBindings();
  } catch (err) {
    showToast(err.message);
  }
}

export function bindMacroActions() {
  q('#btnRefreshMacros').addEventListener('click', () => {
    refreshMacros()
      .then(() => showToast('宏列表已刷新'))
      .catch((err) => showToast(err.message));
  });

  q('#macroFilter').addEventListener('input', () => {
    refreshMacros().catch((err) => showToast(err.message));
  });

  q('#btnMacroRandomToggle').addEventListener('click', () => {
    state.runtimeProfile.random_enabled = !state.runtimeProfile.random_enabled;
    syncRuntimeProfileUI();
  });

  q('#btnLowCpuModeToggle').addEventListener('click', () => {
    state.runtimeProfile.low_cpu_enabled = !state.runtimeProfile.low_cpu_enabled;
    syncRuntimeProfileUI();
  });

  q('#btnTimePeriodToggle').addEventListener('click', () => {
    state.runtimeProfile.low_cpu_time_period = !state.runtimeProfile.low_cpu_time_period;
    syncRuntimeProfileUI();
  });

  q('#btnApplyRuntimeProfile').addEventListener('click', () => {
    applyMacroRuntimeProfile().catch((err) => showToast(err.message));
  });

  q('#btnCreateMacro').addEventListener('click', async () => {
    const name = window.prompt('输入新宏文件名（会创建在「宏/录制宏」目录下）。\n点「确定」按这个名字创建；点「取消」放弃创建。', '新建宏.json');
    if (!name) return;
    const res = await apiCall('create_macro', '宏/录制宏', name);
    showToast(`已创建：${res.path}`);
    await refreshMacros();
  });

  q('#btnImportMacroText')?.addEventListener('click', () => {
    openImportMacroDialog({
      title: '导入现成宏 → 新建宏文件',
      note: '转换后会新建一个 .json 宏文件，导完可以直接在右边填触发键绑定执行。',
      onRows: async ({ rows }) => {
        const stamp = new Date().toISOString().replace(/[-:]/g, '').replace('T', '_').slice(0, 15);
        const created = await apiCall('create_macro', '宏/导入宏', `导入_${stamp}.json`);
        await apiCall('save_macro', created.path, rows);
        state.selectedMacro = created.path;
        q('#selectedMacroPath').value = created.path;
        await refreshMacros();
        await refreshBindings().catch(() => {});
        if (state.onSelectMacroForEditor) {
          await state.onSelectMacroForEditor(created.path).catch(() => {});
        }
        showToast(`已生成宏文件：${created.path}（${rows.length} 行）`);
      },
    });
  });

  q('#btnDeleteMacro').addEventListener('click', async () => {
    if (!state.selectedMacro) return showToast('请先选择一个宏文件');
    if (!window.confirm(`确认删除宏文件「${state.selectedMacro}」？\n\n点「确定」立即删除，删除后不可恢复；\n点「取消」保留该文件。`)) return;

    await apiCall('delete_macro', state.selectedMacro);
    showToast('已删除宏文件');
    state.selectedMacro = null;
    state.editorRows = [];
    if (state.renderActionList) state.renderActionList();
    await refreshMacros();
    await refreshBindings();
    refreshSchemeTabsQuietly();
  });

  q('#btnCaptureKey').addEventListener('click', async () => {
    showToast('请在 8 秒内按下键盘或鼠标侧键');
    const res = await apiCall('capture_next_binding', 8);
    if (res.canceled) {
      showToast('采集超时，未捕获输入');
      return;
    }
    q('#bindTrigger').value = res.key || '';
    showToast(`已捕获：${res.key}`);
  });

  q('#btnBindMacro').addEventListener('click', async () => {
    const macroPath = state.selectedMacro || q('#selectedMacroPath').value.trim();
    if (!macroPath) return showToast('请先选择宏文件');

    const trigger = q('#bindTrigger').value.trim();
    const repeat = parseNumber(q('#bindRepeat').value, 1);
    const mode = q('#bindMode').value;
    if (!trigger) return showToast('请输入触发键');

    const ret = await apiCall('bind_macro', trigger, macroPath, repeat, mode);
    if (ret.already_bound) {
      showToast(`已存在同名绑定：${trigger}`);
    } else if (ret.replaced_existing && !ret.multi_bind_enabled) {
      showToast(`已覆盖原绑定并重新绑定：${trigger}`);
    } else if (ret.append) {
      showToast(`已追加到触发键：${trigger}`);
    } else {
      showToast(`绑定成功：${trigger}`);
    }
    // 绑定完就把这一条选中，和列表里高亮的行对上
    state.selectedBindingTrigger = String(trigger).trim().toLowerCase();
    await refreshBindings();
    refreshSchemeTabsQuietly();
  });

  q('#btnRunNow').addEventListener('click', async () => {
    const trigger = q('#bindTrigger').value.trim().toLowerCase();
    if (!trigger) return showToast('请先输入触发键');
    await runBindingOnce(trigger);
  });

  async function toggleListeners() {
    try {
      const ret = await apiCall('toggle_global_listeners');
      await refreshListenerButtonState();
      if (ret.running) {
        showToast('正在启动全局监听');
        return;
      }
      showToast('已请求关闭全局监听');
    } catch (err) {
      showToast(err.message);
    }
  }

  async function togglePause() {
    try {
      const ret = await apiCall('toggle_global_pause');
      showToast(ret.paused ? '全局暂停已开启' : '全局暂停已解除');
      await Promise.all([refreshSnapshot(), refreshListenerButtonState()]);
    } catch (err) {
      showToast(err.message);
    }
  }

  q('#btnStartListeners').addEventListener('click', toggleListeners);
  q('#btnGlobalPause').addEventListener('click', togglePause);
  q('#bindingListenBadge').addEventListener('click', (event) => {
    if (event.currentTarget.classList.contains('paused')) return togglePause();
    return toggleListeners();
  });

  // 按键嗅探：直接回答「我按侧键，软件到底收到没有」
  q('#btnSniffInputs')?.addEventListener('click', async () => {
    showToast('请在 6 秒内按几下鼠标侧键', 5000);
    try {
      const ret = await apiCall('sniff_inputs', 6);
      const side = Number(ret.side_hits || 0);
      const mouseCount = Number(ret.mouse_count || 0);
      if (side > 0) {
        showToast(
          `检测到 ${side} 个侧键事件：${(ret.side_detail || []).join('、')}。可查看旁边的热键状态，再核对该绑定的触发模式。`,
          10000,
        );
      } else if (mouseCount > 0) {
        showToast(
          `收到 ${mouseCount} 个鼠标事件，其中没有侧键事件。请检查鼠标驱动是否将侧键映射为后退或前进键。`,
          11000,
        );
      } else {
        showToast(
          '6 秒内未检测到鼠标事件',
          11000,
        );
      }
    } catch (err) {
      showToast(err.message);
    }
  });

  // 换触发模式时，在下拉框下面提示这个模式到底怎么响应按下/抬起
  q('#bindMode')?.addEventListener('change', syncBindModeHint);
  syncBindModeHint();

  q('#btnStopAll').addEventListener('click', async () => {
    await apiCall('stop_all_macros');
    showToast('已停止全部宏');
    await refreshBindings();
  });

  q('#btnImportFolder').addEventListener('click', async () => {
    const pick = await apiCall('choose_import_folder');
    if (pick.canceled) return;

    const ret = await apiCall('import_macros_from_folder', pick.path);
    showToast(`已导入 ${ret.count} 个宏文件`);
    await refreshMacros();
  });
}
