import { state, q, qa, showToast, apiCall } from './modules/app-core.js';
import { renderGlobalParams, bindParamsActions } from './modules/params.js';
import {
  refreshMacros,
  refreshBindings,
  refreshMacroRuntimeProfile,
  renderMacroList,
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
import { pollConsole, clearConsole } from './modules/console.js';
import { refreshSnapshot, refreshListenerButtonState } from './modules/dashboard.js';
import { installErrorLogging } from './modules/core/error-log.js';

installErrorLogging('main');

state.renderEditorMacroBrowser = renderEditorMacroBrowser;
state.renderRecordMacroBrowser = renderRecordMacroBrowser;
state.renderActionList = renderActionList;
state.onSelectMacroForEditor = loadMacroToEditor;

function setAutomationEditorShellFullscreen(enabled) {
  const body = document.body;
  body.classList.toggle('automation-editor-fullscreen', !!enabled);
}

async function switchTab(tabName) {
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

  if (tabName === 'params') {
    await renderGlobalParams();
  }
  if (tabName === 'macro') {
    // 先定方案，再列绑定：绑定列表显示的是「当前方案」的内容
    await refreshSchemes().catch((err) => showToast(`加载方案失败：${err.message}`));
    await refreshMacros().catch((err) => showToast(`加载宏列表失败：${err.message}`));
    await refreshBindings().catch(() => {});
    await refreshMacroRuntimeProfile().catch(() => {});
  }
  if (tabName === 'editor') {
    // 录制与化简已经并进这一页：切过来时把录制状态、宏文件夹一起刷好
    await refreshMacros().catch((err) => showToast(`加载宏列表失败：${err.message}`));
    await refreshRecordControls();
    renderRecordMacroBrowser();
  }
}

function bindTabs() {
  q('#tabNav').addEventListener('click', (e) => {
    const btn = e.target.closest('.tab-btn');
    if (!btn) return;
    switchTab(btn.dataset.tab).catch((err) => showToast(err.message));
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
      console.log('API 就绪，耗时', i * intervalMs, 'ms');
      return true;
    }
    await new Promise((r) => setTimeout(r, intervalMs));
  }
  console.warn('等待 API 超时，继续初始化');
  return false;
}

async function bootstrap() {
  if (state.bootstrapDone) return;
  state.bootstrapDone = true;
  console.log('开始初始化…');

  try { bindTabs(); console.log('bindTabs 绑定完成'); } catch(e) { console.error('bindTabs 绑定失败：', e); }
  try { bindAutomationFullscreenBridge(); console.log('全屏消息桥接完成'); } catch(e) { console.error('全屏消息桥接失败：', e); }
  try { bindParamsActions(); console.log('参数面板按钮绑定完成'); } catch(e) { console.error('参数面板按钮绑定失败：', e); }
  try { bindMacroActions(); console.log('宏页按钮绑定完成'); } catch(e) { console.error('宏页按钮绑定失败：', e); }
  try { bindSchemeActions(); console.log('方案标签绑定完成'); } catch(e) { console.error('方案标签绑定失败：', e); }
  try { bindEditorActions(); console.log('编辑器按钮绑定完成'); } catch(e) { console.error('编辑器按钮绑定失败：', e); }
  try { bindRecorderActions(); console.log('录制页按钮绑定完成'); } catch(e) { console.error('录制页按钮绑定失败：', e); }

  try {
    const { ACTION_FORM_DEFAULTS } = await import('./modules/core/constants.js');
    console.log('常量已加载');
    const type = q('#actionType').value || 'kd';
    const defaults = ACTION_FORM_DEFAULTS[type] || ACTION_FORM_DEFAULTS.wait;
    const arg1 = q('#actionArg1');
    const arg2 = q('#actionArg2');
    if (arg1 && !arg1.value.trim()) arg1.value = defaults.arg1;
    if (arg2 && !arg2.value.trim()) arg2.value = defaults.arg2;
    console.log('动作表单默认值已写入');
  } catch(e) { console.error('常量或动作表单初始化失败：', e); }

  try {
    q('#btnClearConsole').addEventListener('click', async () => { await clearConsole(); });
    console.log('清空控制台按钮已绑定');
  } catch(e) { console.error('清空控制台按钮绑定失败：', e); }

  await waitForApi();

  try {
    await switchTab('macro');
    console.log('已切换到宏标签页');
  } catch(e) { console.error('切换标签页失败：', e); }
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
    showToast('当前不在 PyWebView 环境，界面仅做静态预览');
  }, 800);
});

function pollDashboard() {
  if (state.currentTab === 'macro') {
    refreshBindings().catch(() => {});
  }
  refreshSnapshot().catch(() => {});
  refreshListenerButtonState().catch(() => {});
}

function startPolling() {
  if (state.pollTimerId) return;
  console.log('启动轮询定时器…');
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
