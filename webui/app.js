import { state, q, qa, showToast } from './modules/app-core.js';
import { renderGlobalParams, bindParamsActions } from './modules/params.js';
import {
  refreshMacros,
  refreshBindings,
  refreshMacroRuntimeProfile,
  bindMacroActions,
} from './modules/macro-bind.js';
import { refreshSchemes, bindSchemeActions } from './modules/macro-schemes.js';
import {
  loadMacroToEditor,
  renderEditorMacroBrowser,
  renderActionList,
  bindEditorActions,
} from './modules/editor/index.js';
import {
  refreshRecordControls,
  renderRecordMacroBrowser,
  bindRecorderActions,
} from './modules/recorder.js';
import { pollConsole, bindConsoleActions } from './modules/console.js';
import { refreshSnapshot, refreshListenerButtonState } from './modules/dashboard.js';
import { installErrorLogging } from './modules/core/error-log.js';

installErrorLogging('main');

state.renderEditorMacroBrowser = renderEditorMacroBrowser;
state.renderRecordMacroBrowser = renderRecordMacroBrowser;
state.renderActionList = renderActionList;
state.onSelectMacroForEditor = loadMacroToEditor;

const TAB_META = {
  macro: { title: '绑定与方案', context: () => state.activeSchemeName ? `当前方案：${state.activeSchemeName}` : '触发键与方案管理' },
  editor: { title: '宏工作台', context: () => state.selectedMacro || '未选择宏文件 · 可直接拖拽积木块编排' },
  automation: { title: '节点自动化', context: () => '可视化流程画布与 AI 搭建' },
  params: { title: '全局设置', context: () => '执行引擎、录制与轨迹参数' },
  console: { title: '系统控制台', context: () => '实时运行状态与诊断输出' },
};

export function syncTopbarHeader() {
  const meta = TAB_META[state.currentTab] || TAB_META.macro;
  const titleEl = q('#topbarTitle');
  const ctxEl = q('#topbarContext');
  if (titleEl) titleEl.textContent = meta.title;
  if (ctxEl) ctxEl.textContent = typeof meta.context === 'function' ? meta.context() : meta.context;
}

function setAutomationEditorShellFullscreen(enabled) {
  document.body.classList.toggle('automation-editor-fullscreen', !!enabled);
}

export async function switchTab(tabName) {
  state.currentTab = tabName;
  if (tabName !== 'automation') {
    setAutomationEditorShellFullscreen(false);
    const frame = q('#automationFrame');
    if (frame && frame.contentWindow) {
      frame.contentWindow.postMessage({
        source: 'macro-forge-host',
        type: 'host-force-exit-fullscreen',
      }, '*');
    }
  }

  qa('.tab-btn').forEach((btn) => {
    btn.classList.toggle('active', btn.dataset.tab === tabName);
  });
  qa('.tab-page').forEach((page) => {
    page.classList.toggle('active', page.dataset.tabPage === tabName);
  });
  syncTopbarHeader();

  if (tabName === 'params') {
    await renderGlobalParams();
  }
  if (tabName === 'macro') {
    await refreshSchemes().catch((err) => showToast(`加载方案失败：${err.message}`));
    await refreshMacros().catch((err) => showToast(`加载宏列表失败：${err.message}`));
    await refreshBindings().catch(() => {});
    await refreshMacroRuntimeProfile().catch(() => {});
    syncTopbarHeader();
  }
  if (tabName === 'editor') {
    await refreshMacros().catch((err) => showToast(`加载宏列表失败：${err.message}`));
    await refreshRecordControls();
    renderRecordMacroBrowser();
    syncTopbarHeader();
  }
}

state.switchTab = switchTab;
state.syncTopbarHeader = syncTopbarHeader;

function bindTabs() {
  q('#tabNav')?.addEventListener('click', (e) => {
    const btn = e.target.closest('.tab-btn');
    if (!btn) return;
    switchTab(btn.dataset.tab).catch((err) => showToast(err.message));
  });
}

function bindSidebarCollapse() {
  const shell = q('#appShell');
  const btn = q('#btnToggleSidebar');
  if (!shell || !btn) return;
  const KEY = 'macrodriver-sidebar-collapsed';
  try {
    if (localStorage.getItem(KEY) === '1') shell.classList.add('sidebar-collapsed');
  } catch { /* 忽略 */ }

  btn.addEventListener('click', () => {
    const collapsed = shell.classList.toggle('sidebar-collapsed');
    try { localStorage.setItem(KEY, collapsed ? '1' : '0'); } catch { /* 忽略 */ }
  });
}

function bindAutomationFullscreenBridge() {
  window.addEventListener('message', (event) => {
    const data = event && event.data;
    if (!data || typeof data !== 'object') return;
    if (data.source !== 'automation-editor') return;
    if (data.type !== 'fullscreen-change') return;
    if (state.currentTab !== 'automation') return;
    setAutomationEditorShellFullscreen(!!data.enabled);
  });
}

async function waitForApi(maxRetries = 20, intervalMs = 300) {
  for (let i = 0; i < maxRetries; i++) {
    if (window.pywebview && window.pywebview.api && typeof window.pywebview.api.health === 'function') {
      return true;
    }
    await new Promise((r) => setTimeout(r, intervalMs));
  }
  return false;
}

async function bootstrap() {
  if (state.bootstrapDone) return;
  state.bootstrapDone = true;

  bindSidebarCollapse();
  bindTabs();
  bindAutomationFullscreenBridge();
  bindParamsActions();
  bindMacroActions();
  bindSchemeActions();
  bindEditorActions();
  bindRecorderActions();
  bindConsoleActions();

  try {
    const { ACTION_FORM_DEFAULTS } = await import('./modules/core/constants.js');
    const type = q('#actionType')?.value || 'kd';
    const defaults = ACTION_FORM_DEFAULTS[type] || ACTION_FORM_DEFAULTS.wait;
    const arg1 = q('#actionArg1');
    const arg2 = q('#actionArg2');
    if (arg1 && !arg1.value.trim()) arg1.value = defaults.arg1;
    if (arg2 && !arg2.value.trim()) arg2.value = defaults.arg2;
  } catch (e) { console.error('动作表单初始化失败：', e); }

  await waitForApi();

  try {
    await switchTab('macro');
  } catch (e) { console.error('切换标签页失败：', e); }

  await Promise.all([refreshSnapshot(), refreshListenerButtonState()]);
  startPolling();
}

window.addEventListener('pywebviewready', () => {
  bootstrap().catch((err) => showToast(err.message || String(err)));
});

window.addEventListener('DOMContentLoaded', () => {
  setTimeout(() => {
    if (window.pywebview && window.pywebview.api) {
      if (!state.bootstrapDone) {
        bootstrap().catch((err) => showToast(err.message || String(err)));
      }
      return;
    }
    showToast('当前不在桌面宿主环境，界面仅做静态预览');
  }, 800);
});

function startPolling() {
  if (state.pollTimerId) return;
  state.pollTimerId = setInterval(() => {
    pollConsole();
  }, 500);
  state.snapshotTimerId = setInterval(() => {
    refreshSnapshot();
  }, 1000);
  state.bindingTimerId = setInterval(() => {
    if (state.currentTab === 'macro') {
      refreshBindings().catch(() => {});
    }
  }, 900);
  state.listenerTimerId = setInterval(() => {
    refreshListenerButtonState();
  }, 1500);
}
