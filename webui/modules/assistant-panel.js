import { apiCall } from './core/api.js';

const PHASES = {
  preparing: '整理本轮需求',
  connecting: '连接模型',
  reasoning: '模型正在思考',
  reading_context: '读取编辑器快照',
  looking_up_nodes: '查找原生节点定义',
  writing: '整理草稿',
  validating: '在本机校验草稿',
  repairing: '根据校验结果修订',
};
const EFFORT = { minimal: '最低', low: '低', medium: '中', high: '高', xhigh: '很高', max: '最高' };
const REQUEST_LIMIT = 6000;
const keyOf = (snapshot) => JSON.stringify(snapshot);
const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function append(parent, tag, text = '', className = '') {
  const element = document.createElement(tag);
  element.className = className;
  element.textContent = text;
  parent.append(element);
  return element;
}

function readEffort(model) {
  try { return localStorage.getItem('macro-ai-effort:' + model); }
  catch { return null; }
}

function saveEffort(model, effort) {
  try { localStorage.setItem('macro-ai-effort:' + model, effort); }
  catch { /* 继续使用本次选择 */ }
}

export function mountAssistant(button, editor) {
  if (!button) return;
  const panel = document.createElement('aside');
  panel.id = 'ai-assistant';
  panel.className = 'ai-panel';
  panel.hidden = true;
  panel.setAttribute('aria-label', 'AI 助手');
  panel.innerHTML = `
    <header class="ai-header">
      <div class="ai-brand"><span class="ai-mark" aria-hidden="true">✦</span><div><h2>AI 助手</h2><p data-ai="editor"></p></div></div>
      <div class="ai-header-actions">
        <button type="button" class="ai-quiet" data-ai="reset">新对话</button>
        <button type="button" class="ai-quiet" data-ai="settings-toggle" aria-controls="ai-connection" aria-expanded="false">连接</button>
        <button type="button" class="ai-close" data-ai="close" aria-label="关闭 AI 助手">×</button>
      </div>
    </header>
    <div class="ai-modelbar">
      <label class="ai-model-label"><span data-ai="model-status">模型</span><select data-ai="model" aria-label="模型"></select></label>
      <div class="ai-mode" data-ai="modes" role="group" aria-label="生成模式">
        <button type="button" data-ai="fast" aria-pressed="false">快速</button>
        <button type="button" data-ai="think" aria-pressed="false">思考</button>
      </div>
      <label class="ai-effort" data-ai="effort-wrap" hidden><span>强度</span><select data-ai="effort" aria-label="思考强度"></select></label>
      <span class="ai-fixed" data-ai="fixed" hidden></span>
    </div>
    <section class="ai-connection" id="ai-connection" data-ai="settings" aria-label="模型连接" hidden>
      <form data-ai="config-form">
        <label>接口地址 <input data-ai="endpoint" type="url" required maxlength="512" placeholder="http://127.0.0.1:8790/v1"></label>
        <label>API 密钥 <input data-ai="key" type="password" maxlength="4096" autocomplete="off" placeholder="由连接提供"></label>
        <p class="ai-muted" data-ai="key-status"></p>
        <label class="ai-checkbox"><input type="checkbox" data-ai="clear-key">清除本次输入的密钥</label>
        <div class="ai-draft-actions">
          <button type="button" data-ai="refresh-models">刷新当前列表</button>
          <button type="submit" class="ai-primary">保存连接</button>
        </div>
        <span data-ai="config-status" role="status"></span>
      </form>
    </section>
    <nav class="ai-tabs" aria-label="助手视图">
      <button type="button" data-ai="talk-tab" aria-pressed="true">对话</button>
      <button type="button" data-ai="review-tab" aria-pressed="false" disabled>草稿<span class="ai-tab-dot" aria-hidden="true"></span></button>
      <button type="button" class="ai-undo" data-ai="undo" hidden>撤销 AI 修改</button>
    </nav>
    <div class="ai-body" data-ai="body">
      <div data-ai="conversation">
        <div class="ai-empty" data-ai="empty">
          <h3 data-ai="empty-title"></h3>
          <div class="ai-examples" data-ai="examples"></div>
        </div>
        <div class="ai-history" data-ai="history" role="log" aria-label="对话消息"></div>
        <div class="ai-progress" data-ai="progress" role="status" hidden>
          <span class="ai-progress-dot" aria-hidden="true"></span><span data-ai="phase"></span><time data-ai="elapsed"></time>
        </div>
      </div>
      <section class="ai-review" data-ai="review" aria-label="修改预览" hidden>
        <div class="ai-review-heading"><div><span data-ai="target"></span><h3>修改预览</h3></div><span class="ai-review-stats" data-ai="stats"></span></div>
        <p class="ai-review-message" data-ai="summary"></p>
        <ul class="ai-approach" data-ai="approach" hidden></ul>
        <ul class="ai-warnings" data-ai="warnings" hidden></ul>
        <ul class="ai-diff" data-ai="diff"></ul>
      </section>
    </div>
    <div class="ai-review-footer" data-ai="review-footer" hidden>
      <button type="button" data-ai="dismiss">放弃草稿</button>
      <button type="button" class="ai-primary" data-ai="apply"></button>
    </div>
    <form class="ai-composer" data-ai="composer">
      <label class="ai-context"><span>上下文</span><select data-ai="scope" aria-label="上下文"></select></label>
      <div class="ai-inputbox">
        <textarea data-ai="prompt" rows="2" aria-label="给 AI 的需求"></textarea>
        <div class="ai-compose-actions">
          <span data-ai="activity" class="ai-activity" role="status"></span>
          <button type="button" data-ai="stop" hidden>停止</button>
          <button type="submit" class="ai-primary" data-ai="send" title="Ctrl+Enter 发送">发送 <span aria-hidden="true">↑</span></button>
        </div>
      </div>
      <p class="ai-privacy">所选上下文和近期对话将发送给当前模型。</p>
    </form>`;

  const host = editor.mode === 'graph' ? document.getElementById('workspace') : button.closest('[data-tab-page]');
  (host || document.body).append(panel);
  const control = (name) => panel.querySelector(`[data-ai="${name}"]`);
  const prompt = control('prompt');
  const scopeSelect = control('scope');
  const modelSelect = control('model');
  const effortSelect = control('effort');
  const activity = control('activity');
  const body = control('body');
  const settings = control('settings');
  let history = [];
  let draft = null;
  let previous = null;
  let active = null;
  let mode = 'auto';
  let level = '';
  let savedModel = '';
  let catalog = {};
  let view = 'talk';
  let connection = null;
  let loading = null;
  let settingsBusy = false;

  control('editor').textContent = editor.title;
  control('apply').textContent = editor.applyLabel;
  control('empty-title').textContent = editor.emptyTitle;
  prompt.placeholder = editor.placeholder;
  for (const [value, label] of editor.scopes) scopeSelect.add(new Option(label, value));
  for (const [label, text, scope] of editor.examples) {
    const example = append(control('examples'), 'button', label);
    example.type = 'button';
    append(example, 'span', '↗', 'ai-example-arrow').setAttribute('aria-hidden', 'true');
    example.addEventListener('click', () => {
      if (active) return;
      scopeSelect.value = scope;
      prompt.value = text;
      prompt.focus({ preventScroll: true });
    });
  }

  function setStatus(text, error = false) {
    activity.textContent = text;
    activity.classList.toggle('ai-error', error);
  }

  function setConnectionStatus(text, error = false) {
    const status = control('config-status');
    status.textContent = text;
    status.classList.toggle('ai-error', error);
  }

  function showView(next) {
    view = next === 'review' && draft ? 'review' : 'talk';
    const review = view === 'review';
    control('talk-tab').setAttribute('aria-pressed', String(!review));
    control('review-tab').setAttribute('aria-pressed', String(review));
    control('review-tab').disabled = !draft;
    control('review').hidden = !review;
    control('review-footer').hidden = !review;
    control('conversation').hidden = review;
    body.scrollTop = review ? 0 : body.scrollHeight;
  }

  function renderReasoning(useSaved = false) {
    const capability = catalog[modelSelect.value] || {};
    const efforts = capability.efforts || [];
    const preference = useSaved && readEffort(modelSelect.value);
    if (useSaved) {
      mode = preference === 'off' && capability.can_disable ? 'off' : 'auto';
      level = efforts.includes(preference) ? preference : capability.default_effort || efforts[0] || '';
    }
    const supported = capability.supports_reasoning;
    const fast = control('fast');
    const think = control('think');
    fast.disabled = Boolean(active) || !capability.can_disable;
    fast.title = capability.can_disable ? '关闭本次请求的模型思考' : '当前模型未开放关闭思考';
    think.disabled = Boolean(active) || !supported;
    fast.setAttribute('aria-pressed', String(mode === 'off'));
    think.setAttribute('aria-pressed', String(Boolean(supported) && mode !== 'off'));
    const fixed = control('fixed');
    fixed.textContent = capability.fixed_effort ? '固定 · ' + (EFFORT[capability.fixed_effort] || capability.fixed_effort)
      : efforts.length === 1 && !capability.can_disable ? '仅支持' + (EFFORT[efforts[0]] || efforts[0]) + '强度'
        : !supported && modelSelect.value ? '模型未公开思考选项' : '';
    fixed.hidden = !fixed.textContent;
    control('effort-wrap').hidden = mode === 'off' || efforts.length < 2;
    effortSelect.disabled = Boolean(active);
    effortSelect.replaceChildren(...efforts.map((value) => new Option(EFFORT[value] || value, value, false, value === level)));
  }

  function setBusy() {
    const pending = Boolean(active);
    control('send').hidden = pending;
    control('stop').hidden = !pending;
    control('send').disabled = settingsBusy;
    control('reset').disabled = pending;
    control('apply').disabled = pending || !draft?.changed;
    control('dismiss').disabled = pending;
    control('undo').disabled = pending;
    scopeSelect.disabled = pending;
    modelSelect.disabled = pending || settingsBusy || !modelSelect.value;
    control('settings-toggle').disabled = pending;
    for (const setting of control('config-form').querySelectorAll('button')) setting.disabled = pending || settingsBusy;
    for (const example of control('examples').querySelectorAll('button')) example.disabled = pending;
    renderReasoning();
  }

  function showSettings(open) {
    settings.hidden = !open;
    control('settings-toggle').setAttribute('aria-expanded', String(open));
  }

  function displayConnection(data) {
    connection = data;
    control('endpoint').value = data.endpoint || '';
    control('key').value = '';
    control('clear-key').checked = false;
    const labels = { workbuddy: '已使用 WorkBuddy 本机密钥', environment: '已使用启动环境密钥', session: '已使用本次会话密钥' };
    control('key-status').textContent = labels[data.key_source] || '界面输入的密钥仅保留到软件退出。';
    control('model-status').textContent = '模型';
  }

  function displayCatalog(data) {
    catalog = data.capabilities || {};
    savedModel = data.saved_model || data.model;
    const names = data.models.map((id) => catalog[id]?.name || id);
    const duplicates = new Set(names.filter((name, i) => names.indexOf(name) < i));
    modelSelect.replaceChildren(...data.models.map((id, i) => {
      const label = duplicates.has(names[i]) ? `${names[i]} · ${id}` : names[i];
      return new Option(label, id, false, id === data.model);
    }));
    renderReasoning(true);
    if (data.saved_model && data.saved_model !== data.model && connection) {
      setConnectionStatus('原模型已不在目录中，发送时会使用当前选择。');
    } else {
      setConnectionStatus(`已读取 ${data.models.length} 个模型`);
    }
  }

  async function loadConnection(saveConnection = false) {
    if (settingsBusy) return;
    settingsBusy = true;
    setBusy();
    setConnectionStatus('正在读取模型列表…');
    try {
      if (saveConnection) {
        const url = control('endpoint').value.trim();
        const key = control('key').value;
        const clear = control('clear-key').checked;
        const { data } = await apiCall('assistant_models', url, key, clear);
        const result = await apiCall('assistant_configure', url, data.model, key, clear);
        displayConnection(result.data);
      } else {
        displayConnection((await apiCall('assistant_settings')).data);
        if (!connection.endpoint) {
          showSettings(true);
          setConnectionStatus('填写接口地址后选择「保存连接」。');
          return;
        }
      }
      const { data } = await apiCall('assistant_models');
      displayCatalog(data);
      if (saveConnection) {
        showSettings(false);
        setStatus('已更新连接');
      }
    } catch (error) {
      showSettings(true);
      setConnectionStatus(error.message, true);
    } finally {
      settingsBusy = false;
      setBusy();
    }
  }

  function toggle(open) {
    panel.hidden = !open;
    host?.classList.toggle('ai-dock-open', open);
    button.setAttribute('aria-expanded', String(open));
    if (editor.onVisibility) requestAnimationFrame(() => editor.onVisibility(open));
    if (!open) {
      button.focus({ preventScroll: true });
      return;
    }
    prompt.focus({ preventScroll: true });
    if (!connection && !loading) loading = loadConnection().finally(() => { loading = null; });
  }

  button.setAttribute('aria-controls', panel.id);
  button.setAttribute('aria-expanded', 'false');
  button.addEventListener('click', () => toggle(panel.hidden));
  control('close').addEventListener('click', () => toggle(false));
  control('settings-toggle').addEventListener('click', () => showSettings(settings.hidden));
  control('refresh-models').addEventListener('click', () => loadConnection());
  control('config-form').addEventListener('submit', (event) => {
    event.preventDefault();
    loadConnection(true);
  });
  modelSelect.addEventListener('change', async () => {
    const chosen = modelSelect.value;
    settingsBusy = true;
    renderReasoning(true);
    setBusy();
    try {
      const result = await apiCall('assistant_select_model', chosen);
      savedModel = result.data.model;
      setStatus('');
    } catch (error) {
      modelSelect.value = savedModel;
      renderReasoning(true);
      setStatus(error.message, true);
    } finally {
      settingsBusy = false;
      setBusy();
    }
  });
  control('fast').addEventListener('click', () => {
    mode = 'off';
    saveEffort(modelSelect.value, mode);
    renderReasoning();
  });
  control('think').addEventListener('click', () => {
    mode = 'auto';
    saveEffort(modelSelect.value, level || mode);
    renderReasoning();
  });
  effortSelect.addEventListener('change', () => {
    level = effortSelect.value;
    saveEffort(modelSelect.value, level);
  });

  panel.addEventListener('keydown', (event) => {
    event.stopPropagation();
    if (event.key === 'Escape') {
      event.preventDefault();
      if (settings.hidden) toggle(false);
      else showSettings(false);
    } else if (event.key === 'Enter' && (event.ctrlKey || event.metaKey) && event.target === prompt && !event.isComposing) {
      event.preventDefault();
      control('composer').requestSubmit();
    }
  });

  function addMessage(role, text, meta = '') {
    control('empty').hidden = true;
    const historyView = control('history');
    const item = append(historyView, 'article', '', `ai-message ai-${role}`);
    const heading = append(item, 'header');
    append(heading, 'strong', role === 'user' ? '你' : 'AI 助手');
    if (meta) append(heading, 'span', meta);
    if (role === 'user' && text.length > 420) {
      append(item, 'p', `${text.slice(0, 160)}…\n…${text.slice(-100)}`, 'ai-request-preview');
      const original = append(item, 'details', '', 'ai-request-original');
      append(original, 'summary', '查看完整输入');
      append(original, 'p', text);
    } else {
      append(item, 'p', text);
    }
    if (historyView.children.length > 30) historyView.firstElementChild.remove();
    if (view === 'talk') body.scrollTop = body.scrollHeight;
    return item;
  }

  function showProgress(phases, started) {
    const name = PHASES[phases.at(-1)] || PHASES.preparing;
    control('phase').textContent = name;
    control('elapsed').textContent = Math.floor((Date.now() - started) / 1000) + 's';
    control('progress').hidden = false;
    if (view === 'talk') body.scrollTop = body.scrollHeight;
  }

  function showTrace(message, phases, started) {
    const trace = append(message, 'details', '', 'ai-trace');
    append(trace, 'summary', `处理过程 · ${Math.max(1, Math.round((Date.now() - started) / 1000))} 秒`);
    const list = append(trace, 'ol');
    for (const phase of phases) if (PHASES[phase]) append(list, 'li', PHASES[phase].replace('正在', ''));
  }

  function fillList(name, values) {
    const list = control(name);
    list.replaceChildren();
    for (const value of values) append(list, 'li', value);
    list.hidden = !values.length;
  }

  function showDraft(reply, before, attached, scope) {
    const proposal = editor.propose(reply, before, attached, scope);
    draft = { before, after: proposal.after, attached, changed: proposal.diff.length > 0 };
    control('target').textContent = editor.scopes.find(([key]) => key === scope)?.[1] || editor.title;
    control('stats').textContent = editor.stats(reply);
    control('summary').textContent = reply.message;
    fillList('approach', reply.approach || []);
    fillList('warnings', reply.warnings || []);
    const diff = control('diff');
    diff.replaceChildren();
    for (const line of proposal.diff.slice(0, 120)) append(diff, 'li', line.text, line.type ? `ai-diff-${line.type}` : '');
    if (proposal.diff.length > 120) append(diff, 'li', `另有 ${proposal.diff.length - 120} 项改动`);
    if (!proposal.diff.length) append(diff, 'li', '草稿与当前内容一致。');
    showView('review');
  }

  function clearDraft() {
    draft = null;
    showView('talk');
  }

  function restoreRequest(job) {
    if (prompt.value) return;
    prompt.value = job.input;
    job.message?.remove();
    control('empty').hidden = Boolean(control('history').children.length);
  }

  control('composer').addEventListener('submit', async (event) => {
    event.preventDefault();
    const input = prompt.value;
    const request = input.trim();
    if (!request || active) return;
    if (request.length > REQUEST_LIMIT && [...request].length > REQUEST_LIMIT) {
      setStatus(editor.mode === 'macro'
        ? '输入超过 6000 字。可先「导入现成宏」，再选择「当前宏」作为上下文。'
        : '输入超过 6000 字，请拆成几条需求。', true);
      return;
    }
    if (!modelSelect.value || settingsBusy) {
      setStatus('请先读取并选择一个模型。', true);
      return;
    }
    const scope = scopeSelect.value;
    const effort = mode === 'off' ? 'off' : (level || 'auto');
    const model = modelSelect.value;
    const job = { id: '', input, started: Date.now(), cancelled: false, phases: ['preparing'] };
    active = job;
    setBusy();
    try {
      const before = editor.snapshot();
      const attached = editor.attach(scope, before);
      const scopeLabel = scopeSelect.selectedOptions[0]?.textContent || '';
      job.message = addMessage('user', request, scopeLabel);
      prompt.value = '';
      showView('talk');
      setStatus('');
      showProgress(job.phases, job.started);
      const result = await apiCall('assistant_start', editor.mode, scope, request, attached.data, history, model, effort);
      job.id = result.data.id;
      if (job.cancelled) {
        await apiCall('assistant_cancel', job.id);
        return;
      }
      while (active === job) {
        const { data } = await apiCall('assistant_poll', job.id);
        if (active !== job) return;
        job.phases = data.phases;
        showProgress(job.phases, job.started);
        if (data.state === 'finished') {
          if (!data.result.ok) throw new Error(data.result.error || '生成失败，请重试');
          const reply = data.result.data;
          history = [...history, { role: 'user', content: request }, { role: 'assistant', content: reply.message }].slice(-8);
          const message = addMessage('assistant', reply.message, catalog[model]?.name || model);
          showTrace(message, job.phases, job.started);
          if (reply.kind === 'draft') {
            showDraft(reply, before, attached, scope);
            setStatus('草稿已校验');
          } else {
            setStatus(reply.kind === 'question' ? '补充信息后可继续' : '');
          }
          return;
        }
        if (data.state === 'cancelled') return;
        await wait(250);
      }
    } catch (error) {
      if (active !== job) return;
      restoreRequest(job);
      setStatus(error.message || '生成失败，请重试', true);
    } finally {
      if (active === job) {
        active = null;
        control('progress').hidden = true;
        setBusy();
      }
    }
  });

  control('stop').addEventListener('click', () => {
    if (!active) return;
    const job = active;
    job.cancelled = true;
    active = null;
    restoreRequest(job);
    control('progress').hidden = true;
    setStatus('已停止接收本轮结果');
    setBusy();
    if (job.id) apiCall('assistant_cancel', job.id).catch(() => {});
  });
  control('talk-tab').addEventListener('click', () => showView('talk'));
  control('review-tab').addEventListener('click', () => showView('review'));
  control('reset').addEventListener('click', () => {
    history = [];
    control('history').replaceChildren();
    control('empty').hidden = false;
    clearDraft();
    setStatus('');
    prompt.focus({ preventScroll: true });
  });
  control('dismiss').addEventListener('click', () => {
    clearDraft();
    setStatus('已放弃草稿');
  });
  control('apply').addEventListener('click', () => {
    if (!draft || active) return;
    try {
      if (keyOf(editor.snapshot()) !== keyOf(draft.before) ||
          editor.sameSelection && !editor.sameSelection(draft.attached)) {
        throw new Error('编辑内容或选区已改变，请用当前内容重新生成。');
      }
      const before = draft.before;
      editor.apply(draft.after);
      previous = { before, applied: keyOf(editor.snapshot()) };
      control('undo').hidden = false;
      clearDraft();
      setStatus('已应用到编辑器');
    } catch (error) {
      setStatus(error.message, true);
    }
  });
  control('undo').addEventListener('click', () => {
    if (!previous || active) return;
    try {
      if (keyOf(editor.snapshot()) !== previous.applied) throw new Error('应用后已有改动，请先处理后续编辑。');
      editor.apply(previous.before);
      previous = null;
      control('undo').hidden = true;
      setStatus('已撤销 AI 修改');
    } catch (error) {
      setStatus(error.message, true);
    }
  });
  modelSelect.add(new Option('等待连接…', ''));
  setBusy();
}
