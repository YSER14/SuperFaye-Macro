import { state } from './core/state.js';
import { q, qa, showToast } from './core/dom.js';
import { apiCall } from './core/api.js';
import { renderMacroTreeList } from './macro-tree.js';
import { normalizeMacroRows } from './editor/action-model.js';
import { setSelectedLine, getActionAtPath } from './editor/action-ops.js';
import { fillActionEditorByAction, ensureActionFormDefaultsForType } from './editor/action-form.js';
import { renderActionList } from './editor/action-list-render.js';

function parseNumber(text, fallback = 0) {
  const n = Number(text);
  return Number.isFinite(n) ? n : fallback;
}

function normalizeHotkeyName(raw) {
  return String(raw ?? '').trim().toLowerCase();
}

function ensureRecordServiceRunning(actionLabel = '该操作') {
  if (state.recordServiceRunning) return true;
  showToast(`请先启动录制监听，再执行${actionLabel}`);
  return false;
}

function applyRecordButtonUi() {
  const btn = q('#btnStartRecSvc');
  if (!btn) return;

  const running = Boolean(state.recordServiceRunning);
  btn.textContent = running ? '关闭录制监听' : '启动录制监听';
  btn.classList.toggle('success', running);
}

function applyRecordHotkeyTooltips() {
  const hk = state.recordHotkeys;
  const fmt = (key) => (key ? `快捷键: ${key}` : '快捷键: 已关闭');

  const btnStart = q('#btnStartRecSvc');
  const btnArm = q('#btnArmRec');
  const btnStop = q('#btnStopRec');
  const btnSave = q('#btnSaveRec');
  const btnClear = q('#btnClearRec');

  if (btnStart) btnStart.title = `切换录制监听服务。${fmt(hk.toggle_service)}`;
  if (btnArm) btnArm.title = `开始录制布防。${fmt(hk.arm)}`;
  if (btnStop) btnStop.title = `停止录制。${fmt(hk.stop)}`;
  if (btnSave) btnSave.title = `保存录制结果。${fmt(hk.save)}`;
  if (btnClear) btnClear.title = `清空录制缓存。${fmt(hk.clear)}`;
}

export async function refreshRecordControls() {
  try {
    const [svc, hk] = await Promise.all([
      apiCall('get_recorder_service_state'),
      apiCall('get_record_hotkeys'),
    ]);
    state.recordServiceRunning = Boolean(svc.running);
    state.recordHotkeys = {
      toggle_service: normalizeHotkeyName(hk.toggle_service),
      arm: normalizeHotkeyName(hk.arm),
      stop: normalizeHotkeyName(hk.stop),
      save: normalizeHotkeyName(hk.save),
      clear: normalizeHotkeyName(hk.clear),
    };
    applyRecordButtonUi();
    applyRecordHotkeyTooltips();
    // 进页面先拉一次，能看到缓存里已有的录制内容；监听开着就持续刷新
    if (state.recordServiceRunning) startRecordLivePolling();
    else pollRecordLive();
  } catch (err) {
    console.error(err);
  }
}

// ── 实时录制预览（边录边看）─────────────────────────────────────────────
const REC_LIVE_MAX_ROWS = 260;
const REC_LIVE_POLL_MS = 300;

const recLive = {
  cursor: 0,
  timer: 0,
};

function formatLiveTime(ms) {
  const value = Math.max(0, Number(ms) || 0);
  return value >= 1000 ? `+${(value / 1000).toFixed(3)}s` : `+${Math.round(value)}ms`;
}

function setRecLiveBadge(text, mode) {
  const badge = q('#recLiveBadge');
  if (!badge) return;
  badge.textContent = text;
  badge.className = `rec-live-badge ${mode}`;
}

export function clearRecLiveView() {
  const list = q('#recLiveList');
  if (list) list.innerHTML = '';
  const held = q('#recLiveHeld');
  if (held) held.innerHTML = '';
  const meta = q('#recLiveMeta');
  if (meta) meta.textContent = '按下「开始录制」后，这里会实时列出按键与鼠标动作';
  setRecLiveBadge('空闲', 'idle');
}

const REC_LIVE_ICONS = {
  kd: '↓',
  ku: '↑',
  md: '●',
  mu: '○',
  move: '↗',
};

function appendRecLiveRows(rows) {
  const list = q('#recLiveList');
  if (!list || !rows.length) return;
  // 用户往上翻看历史时别硬拽回底部
  const nearBottom = list.scrollHeight - list.scrollTop - list.clientHeight < 24;
  const frag = document.createDocumentFragment();
  rows.forEach((row) => {
    const kind = row.kind || 'other';
    const item = document.createElement('li');
    item.className = `rec-live-item kind-${kind}`;
    const time = document.createElement('span');
    time.className = 'rec-live-time';
    time.textContent = formatLiveTime(row.t_ms);
    const icon = document.createElement('span');
    icon.className = 'rec-live-icon';
    icon.textContent = REC_LIVE_ICONS[kind] || '•';
    const text = document.createElement('span');
    text.className = 'rec-live-text';
    text.textContent = String(row.text ?? '');
    item.appendChild(time);
    item.appendChild(icon);
    item.appendChild(text);
    frag.appendChild(item);
  });
  list.appendChild(frag);
  while (list.children.length > REC_LIVE_MAX_ROWS) {
    list.removeChild(list.firstElementChild);
  }
  if (nearBottom) list.scrollTop = list.scrollHeight;
}

function applyRecLiveMeta(preview) {
  const recording = Boolean(preview.is_recording);
  const armed = Boolean(preview.is_armed);
  setRecLiveBadge(recording ? '录制中' : armed ? '已布防' : '空闲',
    recording ? 'recording' : armed ? 'armed' : 'idle');

  const parts = [];
  if (recording) parts.push(`时长 ${(Number(preview.elapsed_ms || 0) / 1000).toFixed(1)}s`);
  parts.push(`动作 ${preview.action_count || 0}`);
  parts.push(`轨迹 ${preview.track_count || 0}`);
  const meta = q('#recLiveMeta');
  if (meta) meta.textContent = parts.join(' · ');

  const held = q('#recLiveHeld');
  if (held) {
    held.innerHTML = '';
    (preview.held || []).forEach((name) => {
      const chip = document.createElement('span');
      chip.className = 'rec-live-key';
      chip.textContent = String(name);
      held.appendChild(chip);
    });
  }
}

export async function pollRecordLive() {
  try {
    const ret = await apiCall('get_recording_preview', recLive.cursor, 120);
    const preview = ret.preview || {};
    if (Array.isArray(preview.events) && preview.events.length) {
      appendRecLiveRows(preview.events);
    }
    const cursor = Number(preview.cursor);
    if (Number.isFinite(cursor)) recLive.cursor = cursor;
    applyRecLiveMeta(preview);
  } catch (err) {
    // 旧版后端没有这个接口时别每 300ms 刷一次报错，直接停掉轮询
    const message = String((err && err.message) || err || '');
    if (message.includes('不可用') || message.includes('not found')) {
      stopRecordLivePolling();
      return;
    }
    console.error(err);
  }
}

/** 把游标对齐到「现在」，之后只显示新事件（开始录制时调用，避免把上次的旧数据混进来）。 */
async function syncRecLiveCursor() {
  try {
    const ret = await apiCall('get_recording_preview', 0, 1);
    const cursor = Number((ret.preview || {}).cursor);
    if (Number.isFinite(cursor)) recLive.cursor = cursor;
  } catch (err) {
    console.error(err);
  }
}

export function startRecordLivePolling() {
  if (recLive.timer) return;
  recLive.timer = setInterval(() => {
    // 录制与化简并进了「宏编辑保存」，只有停在这一页才需要刷新实时预览
    if (state.currentTab !== 'editor') return;
    pollRecordLive();
  }, REC_LIVE_POLL_MS);
  pollRecordLive();
}

export function stopRecordLivePolling() {
  if (!recLive.timer) return;
  clearInterval(recLive.timer);
  recLive.timer = 0;
}

function setRecordPathInput(path) {
  const target = q(`#${state.activeRecordPathInput}`) || q('#simplifyInput');
  if (!target) return;
  target.value = path;
  target.dispatchEvent(new Event('change'));
}

export function renderRecordMacroBrowser() {
  const list = q('#recordMacroList');
  renderMacroTreeList({
    container: list,
    tree: state.macroTree,
    viewId: 'record',
    selectedPath: null,
    openedPath: null,
    onFill: (path) => {
      setRecordPathInput(path);
      showToast(`已填入路径: ${path}`);
    },
  });
}

/* ── 并进「宏编辑保存」之后的落地动作：录制 / 化简结果直接进动作列表 ── */

function syncEditorAfterRows(selectPath) {
  state.inlineEditingPathKey = null;
  state.expandedLoopPathKeys = new Set();
  const path = Array.isArray(selectPath) ? selectPath : (state.editorRows.length ? [0] : null);
  setSelectedLine(path, 'action');
  const action = path ? getActionAtPath(path) : null;
  if (action) fillActionEditorByAction(action);
  else ensureActionFormDefaultsForType(q('#actionType')?.value || 'kd', true);
  renderActionList();
}

/** 读一个宏文件，按用户选择替换 / 保持当前动作列表 */
async function loadMacroPathIntoEditor(path, label) {
  const target = String(path || '').trim();
  if (!target) {
    showToast(`请先填写${label}路径`);
    return;
  }

  let rows = [];
  try {
    const data = await apiCall('read_macro', target);
    rows = normalizeMacroRows(data.rows);
  } catch (err) {
    showToast(`读不了${label}：${err.message}`);
    return;
  }

  if (!rows.length) {
    showToast(`${label}里没有可载入的动作`);
    return;
  }

  if (state.editorRows.length) {
    const ok = window.confirm(
      `动作列表里已经有 ${state.editorRows.length} 条动作。\n\n`
      + `点「确定」用${label}里的 ${rows.length} 条替换；点「取消」保持现在的列表。`,
    );
    if (!ok) return;
  }

  state.editorRows = rows;
  syncEditorAfterRows([0]);
  showToast(`已把${label}的 ${rows.length} 条动作载入动作列表`);
}

/** 把这次录制的缓存直接拼进动作列表（不写文件） */
async function appendRecordingToEditor() {
  const kind = q('#recInsertKind')?.value || 'full';
  let ret = null;
  try {
    ret = await apiCall('get_recording_macros', kind);
  } catch (err) {
    showToast(`读取录制动作失败：${err.message}`);
    return;
  }

  const rows = normalizeMacroRows(ret.rows || []);
  if (!rows.length) {
    showToast('录制缓存里还没有动作，先点「启动录制监听」→「开始录制」');
    return;
  }

  let replace = true;
  if (state.editorRows.length) {
    replace = window.confirm(
      `动作列表里已经有 ${state.editorRows.length} 条动作。\n\n`
      + `点「确定」用这次录制的 ${rows.length} 条替换整个列表；\n`
      + '点「取消」追加到末尾。',
    );
  }

  if (replace) {
    state.editorRows = rows;
  } else {
    state.editorRows.push(...rows);
  }

  const selectPath = replace ? [0] : [state.editorRows.length - rows.length];
  syncEditorAfterRows(selectPath);

  const counts = ret.counts || {};
  const stat = `完整 ${counts.full || 0} / 仅按键 ${counts.action || 0} / 仅轨迹 ${counts.track || 0}`;
  showToast(`已${replace ? '替换为' : '追加'} ${rows.length} 条录制动作（缓存 ${stat}），可继续拖块编辑`);
}

export function bindRecorderActions() {
  q('#btnStartRecSvc').addEventListener('click', async () => {
    if (state.recordServiceRunning) {
      await apiCall('stop_recorder_service');
      state.recordServiceRunning = false;
      applyRecordButtonUi();
      stopRecordLivePolling();
      clearRecLiveView();
      showToast('录制监听已关闭');
      return;
    }

    await apiCall('start_recorder_service');
    state.recordServiceRunning = true;
    applyRecordButtonUi();
    startRecordLivePolling();
    showToast('录制监听已启动');
  });

  q('#btnArmRec').addEventListener('click', async () => {
    if (!ensureRecordServiceRunning('开始录制')) return;
    const mode = q('#recordStartMode').value;
    const delay = parseNumber(q('#recordDelay').value, 0.2);
    await apiCall('arm_recording', mode, delay);
    clearRecLiveView();
    await syncRecLiveCursor();
    startRecordLivePolling();
    showToast('录制已布防');
  });

  q('#btnStopRec').addEventListener('click', async () => {
    if (!ensureRecordServiceRunning('停止录制')) return;
    await apiCall('stop_recording');
    await pollRecordLive();
    showToast('录制已停止');
  });

  q('#btnSaveRec').addEventListener('click', async () => {
    if (!ensureRecordServiceRunning('保存录制')) return;
    const targets = await apiCall('get_recording_output_targets');
    const existsAndHasContent = (targets.items || []).filter((item) => item.has_content);
    if (existsAndHasContent.length) {
      const msg = existsAndHasContent
        .map((item) => `${item.path} (${item.row_count} 条)`).join('\n');
      const ok = window.confirm(`将覆盖以下已有录制文件:\n${msg}\n\n是否继续保存？`);
      if (!ok) return;
    }
    await apiCall('save_recording');
    showToast('录制结果已保存');
    const { refreshMacros } = await import('./macro-bind.js');
    await refreshMacros();
  });

  q('#btnClearRec').addEventListener('click', async () => {
    if (!ensureRecordServiceRunning('清空缓存')) return;
    await apiCall('clear_recording_buffer');
    clearRecLiveView();
    showToast('录制缓存已清空');
  });

  /* 录制 → 动作列表（不写文件） */
  q('#btnRecToEditor').addEventListener('click', appendRecordingToEditor);

  /* 化简 / 合并结果 → 动作列表 */
  q('#btnSimplifyToEditor').addEventListener('click', () => {
    loadMacroPathIntoEditor(q('#simplifyOutput').value, '化简输出');
  });

  q('#btnMergeToEditor').addEventListener('click', () => {
    loadMacroPathIntoEditor(q('#mergeOutput').value, '合并输出');
  });

  q('#btnUseCurrentMacro').addEventListener('click', () => {
    const path = state.selectedMacro;
    if (!path) return showToast('还没有选中宏文件：先在下方宏文件夹里点一个');
    q('#simplifyInput').value = path;
    if (!q('#simplifyOutput').value.trim()) {
      q('#simplifyOutput').value = String(path).replace(/\.json$/i, '_轨迹化简.json');
    }
    showToast(`输入宏已填：${path}`);
  });

  q('#btnFillMergeFromRecording').addEventListener('click', async () => {
    const ret = await apiCall('get_recording_output_targets');
    const items = ret.items || [];
    const pick = (kind) => items.find((item) => item.kind === kind);
    const action = pick('action');
    const track = pick('track');
    if (!action && !track) return showToast('拿不到录制输出路径');
    if (action) q('#mergeAction').value = action.path;
    if (track) q('#mergeTrack').value = track.path;
    const missing = [action, track].filter((item) => item && !item.exists).map((item) => item.path);
    showToast(missing.length
      ? `已填入路径；这些文件还不存在，先点「保存录制」：${missing.join('、')}`
      : '已填入录制输出路径');
  });

  q('#btnSimplify').addEventListener('click', async () => {
    const inputPath = q('#simplifyInput').value.trim();
    const outputPath = q('#simplifyOutput').value.trim();
    const preset = q('#presetSelect').value;
    if (!inputPath || !outputPath) return showToast('请填写输入与输出路径');

    const ret = await apiCall('simplify_macro_with_preset', inputPath, outputPath, preset);
    showToast(`化简完成: ${ret.path}`);
    const { refreshMacros } = await import('./macro-bind.js');
    await refreshMacros();
  });

  q('#btnMerge').addEventListener('click', async () => {
    const actionPath = q('#mergeAction').value.trim();
    const trackPath = q('#mergeTrack').value.trim();
    const outputPath = q('#mergeOutput').value.trim();
    const preset = q('#presetSelect').value;

    if (!actionPath || !trackPath || !outputPath) {
      return showToast('请填写动作宏、轨迹宏和输出路径');
    }

    const ret = await apiCall('merge_action_track_with_preset', actionPath, trackPath, outputPath, preset);
    showToast(`合并完成: ${ret.path}`);
    const { refreshMacros } = await import('./macro-bind.js');
    await refreshMacros();
  });

  q('#btnRefreshRecordMacroBrowser').addEventListener('click', async () => {
    const { refreshMacros } = await import('./macro-bind.js');
    await refreshMacros();
    showToast('录制区宏文件夹已刷新');
  });

  qa('.record-path-input').forEach((input) => {
    input.addEventListener('focus', () => {
      state.activeRecordPathInput = input.id;
    });

    input.addEventListener('dragover', (e) => {
      e.preventDefault();
      input.classList.add('drop-over');
    });

    input.addEventListener('dragleave', () => {
      input.classList.remove('drop-over');
    });

    input.addEventListener('drop', (e) => {
      e.preventDefault();
      input.classList.remove('drop-over');
      const macroPath = e.dataTransfer.getData('text/macro-path') || e.dataTransfer.getData('text/plain');
      if (!macroPath) return;
      input.value = macroPath;
      input.dispatchEvent(new Event('change'));
      showToast(`已填入路径: ${macroPath}`);
    });
  });
}
