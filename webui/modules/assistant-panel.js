import { apiCall } from './core/api.js';
import { renderRichReply } from './assistant-markdown.js';
import {
  createVoiceRecorder,
  estimateAttachmentTokens,
  extractVideoKeyframes,
  formatDuration,
  iconMarkup,
  openAttachmentLightbox,
  openPickerModal,
  openRegionCropper,
  openUrlInputModal,
  readFileAsDataUrl,
  renderComposerAttachments,
  renderMessageAttachments,
} from './assistant-multimodal.js';

const PHASES = {
  preparing: '整理本轮需求',
  connecting: '连接模型',
  compacting: '正在压缩对话上下文',
  reasoning: '模型正在思考',
  reading_context: '读取编辑器快照',
  listing_archive: '查看 ZIP 内容',
  extracting_archive: '解压 ZIP',
  recognizing_text: '本地识别图片文字',
  inspecting_images: '使用模型识别图片',
  looking_up_nodes: '查找原生节点定义',
  building_workflow: '搭建流程并连接节点',
  writing: '整理草稿',
  validating: '在本机校验草稿',
  repairing: '根据校验结果修订',
};
const EFFORT = { minimal: '最低', low: '低', medium: '中', high: '高', xhigh: '很高', max: '最高' };
const PERMISSIONS = {
  ask: '联网、外部文件操作和高风险写入时审批',
  auto: '自动批准常规操作，外部文件、覆盖、脚本写入、发送图片和有风险的网页访问仍需审批',
  full: '直接执行文件、网页和模型识图工具，文件权限与当前 Windows 用户相同',
};
const keyOf = (snapshot) => JSON.stringify(snapshot);
const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const tokenEncoder = new TextEncoder();
const tokenCount = new Intl.NumberFormat('zh-CN');
const percent = new Intl.NumberFormat('zh-CN', { style: 'percent', maximumFractionDigits: 1 });

function estimateTokens(text) {
  return text ? Math.max([...text].length, Math.ceil(tokenEncoder.encode(text).length / 2)) + 16 : 0;
}

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
  panel.setAttribute('aria-labelledby', 'ai-model-title');
  panel.innerHTML = `
    <div class="ai-drop-overlay" data-ai="drop-overlay" hidden>
      <div class="ai-drop-card">
        <strong>松开以添加多模态输入</strong>
        <span>支持图片、截图、视频、音频、ZIP 压缩包、文档、宏文件</span>
      </div>
    </div>
    <header class="ai-header">
      <div class="ai-heading"><h2 id="ai-model-title" data-ai="model-title">模型</h2></div>
      <div class="ai-header-actions">
        <button type="button" class="ai-quiet" data-ai="reset"><span aria-hidden="true">＋</span>新对话</button>
        <button type="button" class="ai-quiet" data-ai="chats-toggle" aria-controls="ai-chats" aria-expanded="false">历史</button>
        <button type="button" class="ai-icon ai-quiet" data-ai="settings-toggle" aria-label="模型连接" title="模型连接" aria-controls="ai-connection" aria-expanded="false">⚙</button>
        <button type="button" class="ai-icon ai-quiet" data-ai="close" aria-label="关闭对话" title="关闭对话">×</button>
      </div>
    </header>
    <section class="ai-connection ai-popover ai-chats" id="ai-chats" data-ai="chats" aria-label="本机对话历史" hidden>
      <div data-ai="chat-list"></div>
    </section>
    <section class="ai-connection ai-popover" id="ai-connection" data-ai="settings" aria-label="模型连接" hidden>
      <form data-ai="config-form">
        <label>接口地址 <input data-ai="endpoint" type="url" required maxlength="512" placeholder="https://api.openai.com/v1"></label>
        <label>API 密钥 <input data-ai="key" type="password" maxlength="4096" autocomplete="off" placeholder="由连接提供"></label>
        <p class="ai-muted" data-ai="key-status"></p>
        <label class="ai-checkbox"><input type="checkbox" data-ai="clear-key">清除本次输入的密钥</label>
        <div class="ai-draft-actions">
          <button type="button" data-ai="refresh-models">刷新当前列表</button>
          <button type="submit" class="ai-primary">保存连接</button>
        </div>
        <span data-ai="config-status" role="status"></span>
        <p class="ai-muted">对话保存在本机。近期消息和模型读取的内容会发送给当前模型。</p>
      </form>
    </section>
    <div class="ai-body" data-ai="body">
      <div class="ai-empty" data-ai="empty">
        <h3 data-ai="empty-title"></h3>
        <p class="ai-empty-description" data-ai="empty-description" hidden></p>
        <div class="ai-examples" data-ai="examples"></div>
        <p class="ai-muted ai-empty-multimodal">支持 Ctrl+V 粘贴截图、拖入图片/ZIP/文件、屏幕框选、语音录音、视频抽帧与引用宏</p>
      </div>
      <div class="ai-history" data-ai="history" role="log" aria-label="对话消息"></div>
      <details class="ai-review" data-ai="review" hidden>
        <summary><strong data-ai="review-title">查看修改草稿</strong><span data-ai="stats"></span></summary>
        <div class="ai-review-content">
          <p class="ai-review-target" data-ai="target"></p>
          <ul class="ai-approach" data-ai="approach" hidden></ul>
          <ul class="ai-warnings" data-ai="warnings" hidden></ul>
          <ul class="ai-diff" data-ai="diff"></ul>
          <div class="ai-draft-actions" data-ai="review-actions">
            <button type="button" class="ai-quiet" data-ai="dismiss">放弃草稿</button>
            <button type="button" class="ai-primary" data-ai="apply"></button>
          </div>
        </div>
      </details>
      <div class="ai-progress" data-ai="progress" role="status" hidden>
        <span class="ai-progress-dot" aria-hidden="true"></span><span data-ai="phase"></span><time data-ai="elapsed"></time>
      </div>
    </div>
    <section class="ai-approval" data-ai="approval" aria-label="工具操作审批" hidden>
      <strong data-ai="approval-action"></strong>
      <p data-ai="approval-target"></p>
      <div class="ai-draft-actions">
        <button type="button" data-ai="deny">拒绝</button>
        <button type="button" class="ai-primary" data-ai="allow">批准本次</button>
      </div>
    </section>
    <form class="ai-composer" data-ai="composer">
      <div class="ai-status-row">
        <span data-ai="activity" class="ai-activity" role="status"></span>
        <button type="button" class="ai-quiet" data-ai="undo" hidden>撤销 AI 修改</button>
      </div>
      <div class="ai-inputbox">
        <div class="ai-attachments" data-ai="attachments" hidden></div>
        <div class="ai-recording-bar" data-ai="recording-bar" hidden>
          <span class="ai-rec-dot" aria-hidden="true"></span>
          <span class="ai-rec-label">正在录音</span>
          <time class="ai-rec-time" data-ai="rec-time">00:00</time>
          <span class="ai-rec-transcript" data-ai="rec-transcript"></span>
          <button type="button" class="ai-quiet" data-ai="rec-cancel">取消</button>
          <button type="button" class="ai-primary" data-ai="rec-stop">完成</button>
        </div>
        <textarea data-ai="prompt" rows="2" aria-label="给 AI 的需求"></textarea>
        <input type="file" data-ai="video-input" accept="video/*" hidden>
        <div class="ai-compose-actions">
          <button type="button" class="ai-quiet ai-icon ai-attach-btn" data-ai="attach-toggle" aria-label="添加多模态输入" title="添加图片、截图、视频、音频、文件或宏引用" aria-expanded="false" aria-controls="ai-attach-menu">${iconMarkup('plus', 15)}</button>
          <div class="ai-popover ai-attach-menu" id="ai-attach-menu" data-ai="attach-menu" aria-label="多模态输入" role="group" hidden></div>
          <button type="button" class="ai-quiet ai-icon ai-mic-btn" data-ai="mic-toggle" aria-label="语音输入" title="点击开始录音（再次点击完成）">${iconMarkup('mic', 15)}</button>
          <button type="button" class="ai-quiet ai-icon ai-slash" data-ai="commands-toggle" aria-label="助手命令" title="助手命令 (/)" aria-expanded="false" aria-controls="ai-commands">/</button>
          <div class="ai-popover ai-commands" id="ai-commands" data-ai="commands" aria-label="助手命令" role="group" hidden></div>
          <select class="ai-model" data-ai="model" aria-label="模型"></select>
          <select class="ai-permission" data-ai="permission" aria-label="操作权限">
            <option value="ask">请求批准</option>
            <option value="auto">帮我批准</option>
            <option value="full">完全访问权限</option>
          </select>
          <div class="ai-reasoning">
            <button type="button" class="ai-quiet" data-ai="reasoning-toggle" aria-controls="ai-reasoning" aria-expanded="false">思考</button>
            <div class="ai-reasoning-menu ai-popover" id="ai-reasoning" data-ai="reasoning" role="group" aria-label="模型思考设置" hidden>
              <div class="ai-mode" role="group" aria-label="生成模式">
                <button type="button" data-ai="fast" aria-pressed="false">快速</button>
                <button type="button" data-ai="think" aria-pressed="false">思考</button>
              </div>
              <label class="ai-effort" data-ai="effort-wrap" hidden><span>思考强度</span><select data-ai="effort"></select></label>
              <span class="ai-fixed" data-ai="fixed" hidden></span>
            </div>
          </div>
          <div class="ai-context">
            <button type="button" class="ai-quiet ai-icon ai-context-toggle" data-ai="context-toggle" aria-label="上下文占用" aria-expanded="false" aria-controls="ai-context">
              <span data-ai="context-ring" class="ai-context-ring" aria-hidden="true"></span>
            </button>
            <div class="ai-popover ai-context-menu" id="ai-context" data-ai="context" aria-label="上下文占用" role="group" hidden>
              <div class="ai-context-heading"><strong>上下文占用</strong><strong data-ai="context-percent"></strong></div>
              <div data-ai="context-tokens"></div>
              <p class="ai-muted" data-ai="context-source"></p>
              <p class="ai-muted" data-ai="context-last" hidden></p>
            </div>
          </div>
          <button type="button" class="ai-icon ai-stop" data-ai="stop" aria-label="停止生成" title="停止生成" hidden><span aria-hidden="true">■</span></button>
          <button type="submit" class="ai-primary ai-icon ai-send" data-ai="send" aria-label="发送" title="Enter 发送 · Shift+Enter 换行"><span aria-hidden="true">↑</span></button>
        </div>
      </div>
    </form>`;

  const host = editor.mode === 'graph' ? document.getElementById('workspace') : button.closest('[data-tab-page]');
  (host || document.body).append(panel);
  const control = (name) => panel.querySelector(`[data-ai="${name}"]`);
  const prompt = control('prompt');
  const modelSelect = control('model');
  const permissionSelect = control('permission');
  const effortSelect = control('effort');
  const activity = control('activity');
  const body = control('body');
  const review = control('review');
  const settings = control('settings');
  const chats = control('chats');
  const reasoning = control('reasoning');
  const reasoningToggle = control('reasoning-toggle');
  const commandsMenu = control('commands');
  const attachMenu = control('attach-menu');
  const attachToggle = control('attach-toggle');
  const micToggle = control('mic-toggle');
  const attachmentsTray = control('attachments');
  const recordingBar = control('recording-bar');
  const dropOverlay = control('drop-overlay');
  const videoInput = control('video-input');
  const contextToggle = control('context-toggle');
  const contextMenu = control('context');
  let chat = '';
  let historyTokens = 0;
  let lastUsage = null;
  let selectedCommand = -1;
  let archive = [];
  let attachments = [];
  let recorder = null;
  let recordingBusy = false;
  let captureCancel = null;
  const mediaUrlCache = new Map();
  let dragDepth = 0;
  let chatsLoaded = false;
  let chatsBusy = false;
  let permissionBusy = false;
  let permission = 'ask';
  let approval = null;
  let draft = null;
  let previous = null;
  let active = null;
  let mode = 'auto';
  let level = '';
  let savedModel = '';
  let catalog = {};
  let connection = null;
  let loading = null;
  let settingsBusy = false;
  const commands = [
    ['new', '新建对话', () => loadChats('')],
    ['history', '打开历史对话', () => { showChats(true); loadChats(); }],
    ['capture', '屏幕框选截图', () => runAttachAction('capture-region')],
    ['file', '添加文件或图片', () => runAttachAction('any')],
    ['voice', '语音录音输入', () => toggleVoiceRecording()],
    ['model', '切换模型', () => {
      modelSelect.focus();
      try { modelSelect.showPicker?.(); } catch { /* 保留对下拉框的焦点 */ }
    }],
    ['status', '查看上下文占用', () => showContext(true)],
    ['compact', '压缩上下文，保留原对话', () => submitPrompt('', true)],
    ['stop', '停止本轮生成', () => control('stop').click()],
    ['help', '查看可用命令', () => showCommands(true)],
  ];

  control('apply').textContent = editor.applyLabel;
  if (editor.heading) control('model-title').textContent = editor.heading;
  control('empty-title').textContent = editor.emptyTitle;
  if (editor.emptyDescription) {
    control('empty-description').textContent = editor.emptyDescription;
    control('empty-description').hidden = false;
  }
  prompt.placeholder = editor.placeholder;
  for (const [label, text] of editor.examples) {
    const example = append(control('examples'), 'button', label);
    example.type = 'button';
    append(example, 'span', '↗').setAttribute('aria-hidden', 'true');
    example.addEventListener('click', () => {
      if (active) return;
      prompt.value = text;
      prompt.focus({ preventScroll: true });
    });
  }
  permissionSelect.title = PERMISSIONS[permission];

  function setStatus(text, error = false) {
    activity.textContent = text;
    activity.classList.toggle('ai-error', error);
  }

  function commandQuery() {
    return /^\/([a-z]*)$/i.exec(prompt.value.trim())?.[1].toLowerCase();
  }

  function commandEnabled(name) {
    if (name === 'status' || name === 'help') return true;
    if (name === 'stop') return Boolean(active);
    return !active && !chatsBusy && !settingsBusy && !permissionBusy;
  }

  function showCommands(open, query = commandQuery() ?? '') {
    const toggle = control('commands-toggle');
    const matches = open ? commands.filter(([name]) => name.startsWith(query)) : [];
    commandsMenu.hidden = !matches.length;
    toggle.setAttribute('aria-expanded', String(Boolean(matches.length)));
    if (!matches.length) return;
    showAttachMenu(false);
    showContext(false);
    showReasoning(false);
    showSettings(false);
    showChats(false);
    selectedCommand = -1;
    commandsMenu.replaceChildren();
    for (const [name, label, run] of matches) {
      const option = append(commandsMenu, 'button', '', 'ai-command ai-quiet');
      option.type = 'button';
      option.disabled = !commandEnabled(name);
      append(option, 'code', '/' + name);
      append(option, 'span', label);
      option.addEventListener('click', () => {
        if (commandQuery() !== undefined) prompt.value = '';
        showCommands(false);
        renderContext();
        if (name !== 'model') prompt.focus({ preventScroll: true });
        run();
      });
    }
  }

  async function fetchAttachmentMediaUrl(path) {
    if (!path) return '';
    if (mediaUrlCache.has(path)) return mediaUrlCache.get(path);
    const { data } = await apiCall('assistant_read_attachment', path);
    if (data?.url) {
      mediaUrlCache.set(path, data.url);
      return data.url;
    }
    return '';
  }

  function renderAttachments() {
    const capability = catalog[modelSelect.value] || {};
    renderComposerAttachments(attachmentsTray, attachments, {
      capability,
      onRemove: (id) => {
        attachments = attachments.filter((item) => item.id !== id);
        renderAttachments();
        renderContext();
      },
      onToggleMode: (att) => {
        att.mode = att.mode === 'ocr' ? 'auto' : 'ocr';
        renderAttachments();
        renderContext();
      },
      onPreview: (att) => {
        openAttachmentLightbox(att, {
          fetchMedia: fetchAttachmentMediaUrl,
          editable: true,
          onToggleMode: (target) => {
            target.mode = target.mode === 'ocr' ? 'auto' : 'ocr';
            renderAttachments();
            renderContext();
          },
        });
      },
    });
  }

  function pushAttachments(items) {
    if (!Array.isArray(items) || !items.length) return;
    const room = Math.max(0, 16 - attachments.length);
    if (!room) {
      setStatus('单次对话最多附带 16 个多模态内容', true);
      return;
    }
    const added = items.slice(0, room);
    attachments.push(...added);
    if (items.length > room) setStatus('已达到单次最多 16 个附件上限', true);
    renderAttachments();
    renderContext();
  }

  async function stageItem(payload) {
    const { data } = await apiCall('assistant_stage_attachment', payload);
    if (data?.attachment) pushAttachments([data.attachment]);
    return data?.attachment;
  }

  async function hydrateVideoIfNeeded(att) {
    if (att.kind !== 'video' || (att.frames && att.frames.length)) return att;
    const prefix = 'appdata/assistant-attachments/';
    if (!att.path || !att.path.startsWith(prefix)) return att;
    const mediaUrl = '/__attachments/' + att.path.slice(prefix.length).split('/').map(encodeURIComponent).join('/');
    try {
      setStatus(`正在提取视频关键帧：${att.name}…`);
      const extracted = await extractVideoKeyframes(mediaUrl);
      const { data } = await apiCall('assistant_stage_attachment', {
        kind: 'video',
        name: att.name,
        mime: att.mime,
        size: att.size,
        ...extracted,
      });
      setStatus('');
      return data?.attachment || att;
    } catch {
      setStatus('');
      return att;
    }
  }

  async function handleFileObjects(fileList) {
    if (!fileList?.length || active) return;
    const files = [...fileList];
    const added = [];
    for (const file of files) {
      if (attachments.length + added.length >= 16) {
        setStatus('单次对话最多附带 16 个多模态内容', true);
        break;
      }
      const ext = (file.name.split('.').pop() || '').toLowerCase();
      const isVideo = file.type.startsWith('video/') || ['mp4', 'mov', 'webm', 'mkv', 'avi', 'm4v'].includes(ext);
      if (isVideo) {
        try {
          setStatus(`正在提取视频关键帧：${file.name}…`);
          const extracted = await extractVideoKeyframes(file);
          const { data } = await apiCall('assistant_stage_attachment', {
            kind: 'video',
            name: file.name,
            mime: file.type || 'video/mp4',
            size: file.size || 0,
            ...extracted,
          });
          if (data?.attachment) added.push(data.attachment);
          setStatus('');
          continue;
        } catch (err) {
          if (file.size > 32 * 1024 * 1024) {
            setStatus(`视频 ${file.name} 无法抽帧且超过 32 MB，已跳过`, true);
            continue;
          }
          setStatus(`视频抽帧失败（${err.message}），改为普通文件附带`);
        }
      }
      if (file.size > 32 * 1024 * 1024) {
        setStatus(`${file.name} 超过 32 MB，请用「文件 / ZIP」选择器添加`, true);
        continue;
      }
      try {
        const dataBase64 = await readFileAsDataUrl(file);
        const { data } = await apiCall('assistant_stage_attachment', {
          kind: 'auto',
          name: file.name || 'clipboard.png',
          mime: file.type || '',
          data_base64: dataBase64,
        });
        if (data?.attachment) added.push(data.attachment);
      } catch (err) {
        setStatus(err.message || `添加 ${file.name} 失败`, true);
      }
    }
    pushAttachments(added);
  }

  async function pollCoordinateClick() {
    let aborted = false;
    captureCancel = () => { aborted = true; };
    await apiCall('pick_coordinate', 'screen');
    while (!aborted) {
      await wait(200);
      if (aborted) return null;
      const { data } = await apiCall('get_pick_result');
      if (data && data.status !== 'picking') {
        captureCancel = null;
        if (data.phys_x === undefined) throw new Error('坐标拾取已取消或超时');
        return data;
      }
    }
    return null;
  }

  async function captureAndCrop(fetchCapture) {
    try {
      const capResp = await fetchCapture();
      const cap = capResp?.data;
      if (!cap?.image) throw new Error('未能捕获画面');
      const cropped = await openRegionCropper(cap);
      if (!cropped) return;
      await stageItem(cropped);
      setStatus('');
    } catch (err) {
      setStatus(err.message || '截图失败', true);
    }
  }

  async function runAttachAction(action) {
    if (active || chatsBusy || settingsBusy) return;
    showAttachMenu(false);
    try {
      if (['image', 'audio', 'file', 'any'].includes(action)) {
        const { data } = await apiCall('assistant_pick_files', action);
        if (data?.canceled) return;
        const resolved = [];
        for (const att of data?.attachments || []) {
          resolved.push(await hydrateVideoIfNeeded(att));
        }
        pushAttachments(resolved);
        if (data?.errors?.length) setStatus(data.errors.join('；'), true);
      } else if (action === 'video') {
        videoInput.value = '';
        videoInput.click();
      } else if (action === 'capture-region') {
        setStatus('请切换到目标画面后点击左键开始框选截图（按 Esc 放弃）…');
        const pt = await pollCoordinateClick();
        if (!pt) return;
        await captureAndCrop(() => apiCall('capture_monitor_at_mouse'));
      } else if (action === 'capture-delayed') {
        let cancelled = false;
        captureCancel = () => { cancelled = true; };
        for (let s = 3; s >= 1; s -= 1) {
          if (cancelled) return;
          setStatus(`${s} 秒后截取鼠标所在屏幕，请切换到目标窗口…`);
          await wait(1000);
        }
        captureCancel = null;
        if (cancelled) return;
        await captureAndCrop(() => apiCall('capture_monitor_at_mouse'));
      } else if (action === 'capture-window') {
        const { data: windows } = await apiCall('get_window_list');
        const items = (windows || [])
          .filter((w) => w.title && w.rect?.[2] >= 40 && w.rect?.[3] >= 40)
          .map((w) => ({
            icon: 'window',
            label: w.title,
            detail: `${w.class_name || '窗口'} · ${w.rect[2]}×${w.rect[3]}`,
            value: w,
          }));
        const picked = await openPickerModal({
          title: '选择要截图的窗口',
          placeholder: '搜索窗口标题…',
          items,
          emptyText: '没有找到可截取的可见窗口',
        });
        if (!picked) return;
        await captureAndCrop(() => apiCall('capture_window', picked.value.hwnd));
      } else if (action === 'pick-coord') {
        setStatus('请点击目标屏幕位置以拾取坐标…');
        const pt = await pollCoordinateClick();
        if (!pt) return;
        await stageItem({ kind: 'coord', x: pt.phys_x, y: pt.phys_y });
        setStatus('');
      } else if (action === 'macro') {
        const res = await apiCall('list_macros', '');
        const items = (res.items || []).map((m) => ({
          icon: 'macro',
          label: m.name,
          detail: m.path,
          value: m,
        }));
        const picked = await openPickerModal({
          title: '引用宏文件',
          placeholder: '搜索宏文件名或路径…',
          items,
          emptyText: '宏目录内暂无宏文件',
        });
        if (!picked) return;
        await stageItem({ kind: 'macro', path: picked.value.path });
      } else if (action === 'macro_group') {
        const res = await apiCall('load_group_lib');
        const groups = res.lib?.groups || [];
        const items = groups.map((g) => ({
          icon: 'macro_group',
          label: g.name,
          detail: g.folder || '默认分组',
          badge: `${(g.rows || []).length} 步`,
          value: g,
        }));
        const picked = await openPickerModal({
          title: '引用我的宏组',
          placeholder: '搜索宏组名称…',
          items,
          emptyText: '宏组库里还没有已保存的宏组',
        });
        if (!picked) return;
        await stageItem({ kind: 'macro_group', name: picked.value.name });
      } else if (action === 'auto_script') {
        const { data: scripts } = await apiCall('list_auto_scripts');
        const items = (scripts || []).map((s) => ({
          icon: 'auto_script',
          label: s.name,
          detail: s.path,
          value: s,
        }));
        const picked = await openPickerModal({
          title: '引用自动化脚本',
          placeholder: '搜索 .auto.json 脚本…',
          items,
          emptyText: '自动化脚本目录内暂无脚本',
        });
        if (!picked) return;
        await stageItem({ kind: 'auto_script', path: picked.value.path });
      } else if (action === 'template') {
        const { data: templates } = await apiCall('list_templates');
        const items = (templates || []).map((t) => ({
          icon: 'template',
          label: t.name,
          detail: t.path,
          value: t,
        }));
        const picked = await openPickerModal({
          title: '引用图片模板',
          placeholder: '搜索模板文件名…',
          items,
          emptyText: '图片模板目录内暂无图片',
        });
        if (!picked) return;
        await stageItem({ kind: 'template', path: picked.value.path });
      } else if (action === 'url') {
        const val = await openUrlInputModal();
        if (!val) return;
        await stageItem({ kind: 'url', url: val.url, name: val.name });
      }
    } catch (err) {
      captureCancel = null;
      setStatus(err.message || '添加输入失败', true);
    }
  }

  function showAttachMenu(open) {
    attachMenu.hidden = !open;
    attachToggle.setAttribute('aria-expanded', String(open));
    if (!open) return;
    showCommands(false);
    showContext(false);
    showReasoning(false);
    showSettings(false);
    showChats(false);
    attachMenu.replaceChildren();

    const groups = [
      ['媒体与截图', [
        ['image', 'image', '选择图片', '多选 / 支持 Ctrl+V 贴图'],
        ['scissors', 'capture-region', '屏幕框选截图', '点击目标屏拖动裁剪'],
        ['timer', 'capture-delayed', '延迟 3 秒截屏', '切到游戏后自动截屏'],
        ['window', 'capture-window', '窗口截图', '截取指定可见窗口'],
        ['video', 'video', '选择视频', '自动抽取关键帧分析'],
        ['audio', 'audio', '音频文件', 'WAV / MP3 / OGG / FLAC'],
      ]],
      ['文件与工作区引用', [
        ['file', 'file', '文件 / ZIP', '文档、代码、Office 或压缩包'],
        ['macro', 'macro', '引用宏文件', '从宏文件库选择'],
        ['macro_group', 'macro_group', '引用宏组', '从我的宏组选择'],
        ['auto_script', 'auto_script', '自动化脚本', '引用 .auto.json 流程'],
        ['template', 'template', '图片模板', '引用模板图并供识图'],
        ['coord', 'pick-coord', '拾取坐标', '获取屏幕 X/Y 坐标'],
        ['url', 'url', '网页链接', '附带参考网址'],
      ]],
    ];

    for (const [heading, items] of groups) {
      append(attachMenu, 'div', heading, 'ai-attach-group-title');
      const grid = append(attachMenu, 'div', '', 'ai-attach-grid');
      for (const [iconName, action, title, hint] of items) {
        const btn = append(grid, 'button', '', 'ai-attach-option ai-quiet');
        btn.type = 'button';
        btn.title = hint;
        btn.insertAdjacentHTML('beforeend', iconMarkup(iconName, 14));
        const textWrap = append(btn, 'span', '', 'ai-attach-option-text');
        append(textWrap, 'strong', title);
        append(textWrap, 'small', hint);
        btn.addEventListener('click', () => runAttachAction(action));
      }
    }
  }

  async function toggleVoiceRecording() {
    if (active || chatsBusy || settingsBusy) return;
    if (recordingBusy && recorder) {
      await recorder.stop();
      return;
    }
    const recTime = control('rec-time');
    const recTranscript = control('rec-transcript');
    recTime.textContent = '00:00';
    recTranscript.textContent = '';

    recorder = createVoiceRecorder({
      onTick: (ms) => {
        recTime.textContent = formatDuration(ms);
      },
      onTranscript: (text) => {
        recTranscript.textContent = text ? `「${text}」` : '';
      },
      onFinish: async (result) => {
        recordingBusy = false;
        recorder = null;
        recordingBar.hidden = true;
        micToggle.classList.remove('ai-recording');
        setBusy();
        try {
          const att = await stageItem({
            kind: 'audio',
            name: `语音_${formatDuration(result.duration_ms).replace(':', '-')}.wav`,
            mime: 'audio/wav',
            data_base64: result.data_base64,
            duration_ms: result.duration_ms,
            transcript: result.transcript || '',
          });
          if (att && result.blob) att.local_url = URL.createObjectURL(result.blob);
          setStatus(result.transcript ? '已添加语音输入（含识别文字）' : '已添加语音输入');
        } catch (err) {
          setStatus(err.message || '保存语音失败', true);
        }
      },
      onError: (err) => {
        recordingBusy = false;
        recorder = null;
        recordingBar.hidden = true;
        micToggle.classList.remove('ai-recording');
        setBusy();
        setStatus(err.message || '录音失败', true);
      },
    });

    try {
      await recorder.start();
      recordingBusy = true;
      recordingBar.hidden = false;
      micToggle.classList.add('ai-recording');
      showAttachMenu(false);
      setStatus('');
      setBusy();
    } catch (err) {
      recorder = null;
      recordingBusy = false;
      recordingBar.hidden = true;
      micToggle.classList.remove('ai-recording');
      setStatus(err.message || '无法启动麦克风录音', true);
    }
  }

  function renderContext() {
    const cap = catalog[modelSelect.value] || {};
    const windowTokens = cap.context_window;
    const draftText = commandQuery() === undefined ? prompt.value.trim() : '';
    const imageCapable = cap.image_input !== false;
    const attachTokens = attachments.reduce((sum, att) => sum + estimateAttachmentTokens(att, imageCapable), 0);
    const previous = lastUsage?.model === modelSelect.value && lastUsage?.chat === chat ? lastUsage : null;
    const actual = active ? active.usage : !draftText && !attachTokens && previous;
    const estimate = (active?.estimatedTokens ?? historyTokens) +
      (!active ? estimateTokens(draftText ? JSON.stringify(draftText) : '') + attachTokens : 0);
    const used = actual ? actual.input_tokens + actual.output_tokens : estimate;
    const ratio = windowTokens > 0 ? used / windowTokens : null;
    const rating = ratio === null ? '—' : ratio > 0 && ratio < .001 ? '<0.1%' : percent.format(ratio);
    const ring = control('context-ring');
    ring.style.setProperty('--ai-context-fill', `${Math.min(100, used ? Math.max(.8, (ratio || 0) * 100) : 0)}%`);
    ring.dataset.level = ratio >= .9 ? 'high' : ratio >= .75 ? 'medium' : 'normal';
    control('context-percent').textContent = rating;
    control('context-tokens').textContent = `${tokenCount.format(used)} / ${windowTokens ? tokenCount.format(windowTokens) : '—'} tokens`;
    const assumed = windowTokens && catalog[modelSelect.value]?.context_window_assumed;
    const modLabels = { text: '文本', image: '图片', audio: '音频', video: '视频', file: '文件' };
    const mods = (cap.input_modalities || []).map((m) => modLabels[m] || m).join(' · ');
    control('context-source').textContent = (actual ? (active ? '模型返回的本轮最近一次用量' : '模型返回的上次请求用量')
      : '预估对话与待发送文本') +
      (assumed ? ' · 窗口为默认估算值' : !windowTokens ? ' · 等待模型窗口数据' : '') +
      (mods ? `\n输入支持：${mods}` : '');
    const detail = control('context-last');
    detail.hidden = Boolean(active) || !previous;
    if (previous && !active) {
      detail.textContent = actual ? `续聊文本预估 ${tokenCount.format(historyTokens)} tokens`
        : `上次请求实测 ${tokenCount.format(previous.input_tokens + previous.output_tokens)} tokens`;
    }
    contextToggle.title = `上下文占用 ${rating}${actual ? '' : '（估算）'}`;
    contextToggle.setAttribute('aria-label', contextToggle.title);
  }

  function showContext(open) {
    contextMenu.hidden = !open;
    contextToggle.setAttribute('aria-expanded', String(open));
    if (open) {
      showAttachMenu(false);
      showCommands(false);
      showReasoning(false);
      showSettings(false);
      showChats(false);
      renderContext();
    }
  }

  function setConnectionStatus(text, error = false) {
    const status = control('config-status');
    status.textContent = text;
    status.classList.toggle('ai-error', error);
  }

  function scrollToLatest() {
    body.scrollTop = body.scrollHeight;
  }

  function displayModelName() {
    const name = modelSelect.value ? modelSelect.selectedOptions[0]?.textContent : '';
    modelSelect.title = name || '模型';
    control('model-title').textContent = editor.heading || name || '模型';
    control('model-title').title = name || '';
  }

  function displayPermission(value) {
    permission = Object.hasOwn(PERMISSIONS, value) ? value : 'ask';
    permissionSelect.value = permission;
    permissionSelect.title = PERMISSIONS[permission];
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
    if (useSaved) {
      effortSelect.replaceChildren(...efforts.map((value) => new Option(EFFORT[value] || value, value, false, value === level)));
    }
    const strength = capability.fixed_effort || level;
    reasoningToggle.textContent = mode === 'off' ? '快速' : strength ? `思考 · ${EFFORT[strength] || strength}` : '思考';
    reasoningToggle.disabled = Boolean(active) || settingsBusy || !modelSelect.value;
  }

  function setBusy() {
    const pending = Boolean(active);
    control('send').hidden = pending;
    control('stop').hidden = !pending;
    control('send').disabled = settingsBusy || chatsBusy || permissionBusy || recordingBusy;
    attachToggle.disabled = pending || settingsBusy || chatsBusy || recordingBusy;
    micToggle.disabled = pending || settingsBusy || chatsBusy;
    control('reset').disabled = pending || chatsBusy;
    control('chats-toggle').disabled = pending || chatsBusy;
    control('apply').disabled = pending || !draft?.changed;
    control('dismiss').disabled = pending;
    control('undo').disabled = pending;
    permissionSelect.disabled = pending || permissionBusy || chatsBusy;
    modelSelect.disabled = pending || settingsBusy || !modelSelect.value;
    control('settings-toggle').disabled = pending;
    for (const setting of control('config-form').querySelectorAll('button')) setting.disabled = pending || settingsBusy;
    for (const example of control('examples').querySelectorAll('button')) example.disabled = pending;
    renderReasoning();
    renderAttachments();
    renderContext();
    if (!commandsMenu.hidden) showCommands(true);
  }

  function showSettings(open) {
    settings.hidden = !open;
    control('settings-toggle').setAttribute('aria-expanded', String(open));
    if (open) {
      showAttachMenu(false);
      showCommands(false);
      showContext(false);
      showReasoning(false);
      showChats(false);
    }
  }

  function showReasoning(open) {
    reasoning.hidden = !open;
    reasoningToggle.setAttribute('aria-expanded', String(open));
    if (open) {
      showAttachMenu(false);
      showCommands(false);
      showContext(false);
      showSettings(false);
      showChats(false);
    }
  }

  function showChats(open) {
    chats.hidden = !open;
    control('chats-toggle').setAttribute('aria-expanded', String(open));
    if (open) {
      showAttachMenu(false);
      showCommands(false);
      showContext(false);
      showSettings(false);
      showReasoning(false);
    }
  }

  function renderChats() {
    const list = control('chat-list');
    list.replaceChildren();
    if (!archive.length) append(list, 'p', '暂无本机对话', 'ai-muted');
    for (const entry of archive) {
      const option = append(list, 'button', entry.title, 'ai-quiet');
      option.type = 'button';
      option.setAttribute('aria-pressed', String(entry.id === chat));
      option.title = entry.title;
      option.addEventListener('click', () => loadChats(entry.id));
    }
  }

  async function loadChats(selected) {
    if (chatsBusy || active) return;
    chatsBusy = true;
    setBusy();
    try {
      const { data } = await apiCall('assistant_history', editor.mode, selected);
      archive = data.chats || [];
      displayPermission(data.permission || permission);
      if (data.chat === chat && Number.isInteger(data.context_tokens)) historyTokens = data.context_tokens;
      if (selected !== undefined || !chatsLoaded) {
        clearDraft();
        control('history').replaceChildren();
        control('empty').hidden = false;
        chat = data.chat || '';
        historyTokens = data.context_tokens || 0;
        lastUsage = null;
        for (const turn of data.turns || []) {
          const user = addMessage('user', turn.prompt, '', turn.attachments || []);
          if (turn.reply) {
            const message = addMessage('assistant', turn.reply, turn.model);
            showTrace(message, turn.phases || [], turn.seconds);
            showEffects(message, turn.effects);
          } else {
            showEffects(user, turn.effects);
          }
        }
        if (selected !== undefined) {
          showChats(false);
          setStatus('');
          prompt.focus({ preventScroll: true });
        }
        scrollToLatest();
      }
      chatsLoaded = true;
      renderChats();
    } catch (error) {
      setStatus(error.message, true);
    } finally {
      chatsBusy = false;
      setBusy();
    }
  }

  function displayConnection(data) {
    connection = data;
    control('endpoint').value = data.endpoint || '';
    control('key').value = '';
    control('clear-key').checked = false;
    const labels = { environment: '已使用启动环境密钥', session: '已使用本次会话密钥' };
    control('key-status').textContent = labels[data.key_source] || '界面输入的密钥仅保留到软件退出。';
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
    displayModelName();
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

  function startAssistant() {
    loadChats();
    if (chatsLoaded) scrollToLatest();
    if (!connection && !loading) loading = loadConnection().finally(() => { loading = null; });
  }

  function toggle(open, focus = true) {
    panel.hidden = !open;
    host?.classList.toggle('ai-dock-open', open);
    button.setAttribute('aria-expanded', String(open));
    if (editor.defaultOpen) button.setAttribute('aria-pressed', String(open));
    if (editor.onVisibility) requestAnimationFrame(() => editor.onVisibility(open));
    if (!open) {
      showAttachMenu(false);
      showCommands(false);
      showContext(false);
      showReasoning(false);
      showChats(false);
      if (focus) button.focus({ preventScroll: true });
      return;
    }
    if (focus) prompt.focus({ preventScroll: true });
    if (panel.offsetWidth && window.pywebview?.api) startAssistant();
  }

  button.setAttribute('aria-controls', panel.id);
  button.setAttribute('aria-expanded', 'false');
  button.addEventListener('click', () => toggle(editor.defaultOpen || panel.hidden));
  if (editor.defaultOpen) control('close').hidden = true;
  else control('close').addEventListener('click', () => toggle(false));
  control('settings-toggle').addEventListener('click', () => showSettings(settings.hidden));
  contextToggle.addEventListener('click', () => showContext(contextMenu.hidden));
  control('commands-toggle').addEventListener('click', () => showCommands(commandsMenu.hidden, ''));
  attachToggle.addEventListener('click', () => showAttachMenu(attachMenu.hidden));
  micToggle.addEventListener('click', () => toggleVoiceRecording());
  control('rec-stop').addEventListener('click', () => recorder?.stop());
  control('rec-cancel').addEventListener('click', () => {
    recorder?.cancel();
    recorder = null;
    recordingBusy = false;
    recordingBar.hidden = true;
    micToggle.classList.remove('ai-recording');
    setBusy();
  });
  videoInput.addEventListener('change', () => {
    if (videoInput.files?.length) handleFileObjects(videoInput.files);
    videoInput.value = '';
  });
  prompt.addEventListener('paste', (event) => {
    const dt = event.clipboardData;
    if (!dt) return;
    const files = [];
    if (dt.files?.length) {
      files.push(...dt.files);
    } else if (dt.items?.length) {
      for (const item of dt.items) {
        if (item.kind === 'file') {
          const file = item.getAsFile();
          if (file) files.push(file);
        }
      }
    }
    if (!files.length) return;
    if (!dt.getData('text/plain')) event.preventDefault();
    handleFileObjects(files);
  });
  panel.addEventListener('dragenter', (event) => {
    if (active) return;
    event.preventDefault();
    dragDepth += 1;
    dropOverlay.hidden = false;
  });
  panel.addEventListener('dragover', (event) => {
    if (active) return;
    event.preventDefault();
    event.dataTransfer.dropEffect = 'copy';
  });
  panel.addEventListener('dragleave', () => {
    dragDepth = Math.max(0, dragDepth - 1);
    if (!dragDepth) dropOverlay.hidden = true;
  });
  panel.addEventListener('drop', (event) => {
    event.preventDefault();
    dragDepth = 0;
    dropOverlay.hidden = true;
    if (active) return;
    const dt = event.dataTransfer;
    if (!dt) return;
    const macroPath = dt.getData('text/macro-path');
    if (macroPath && macroPath.toLowerCase().endsWith('.json')) {
      stageItem({ kind: 'macro', path: macroPath }).catch((err) => setStatus(err.message, true));
      return;
    }
    if (dt.files?.length) handleFileObjects(dt.files);
  });
  prompt.addEventListener('input', (event) => {
    if (!event.isComposing) showCommands(commandQuery() !== undefined);
    renderContext();
  });
  document.addEventListener('pointerdown', (event) => {
    if (!attachMenu.hidden && !attachMenu.contains(event.target) &&
        !attachToggle.contains(event.target)) showAttachMenu(false);
    if (!commandsMenu.hidden && !commandsMenu.contains(event.target) &&
        event.target !== prompt && event.target !== control('commands-toggle')) showCommands(false);
    if (!contextMenu.hidden && !control('context-toggle').parentElement.contains(event.target)) showContext(false);
  });
  control('chats-toggle').addEventListener('click', () => {
    const open = chats.hidden;
    showChats(open);
    if (open) loadChats();
  });
  permissionSelect.addEventListener('change', async () => {
    permissionBusy = true;
    setBusy();
    try {
      const { data } = await apiCall('assistant_set_permission', permissionSelect.value);
      displayPermission(data.permission);
    } catch (error) {
      displayPermission(permission);
      setStatus(error.message, true);
    } finally {
      permissionBusy = false;
      setBusy();
    }
  });
  reasoningToggle.addEventListener('click', () => showReasoning(reasoning.hidden));
  panel.addEventListener('focusout', (event) => {
    if (reasoningToggle.parentElement.contains(event.target) && !reasoningToggle.parentElement.contains(event.relatedTarget)) {
      showReasoning(false);
    }
  });
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
      displayModelName();
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
    renderReasoning();
  });

  panel.addEventListener('keydown', (event) => {
    event.stopPropagation();
    if (event.isComposing || event.keyCode === 229) return;
    if (!commandsMenu.hidden && event.target === prompt && ['ArrowUp', 'ArrowDown'].includes(event.key)) {
      const options = [...commandsMenu.querySelectorAll('button:not(:disabled)')];
      if (!options.length) return;
      event.preventDefault();
      const direction = event.key === 'ArrowDown' ? 1 : -1;
      selectedCommand = selectedCommand < 0 ? (direction > 0 ? 0 : options.length - 1)
        : (selectedCommand + direction + options.length) % options.length;
      options.forEach((option, i) => { option.dataset.selected = String(i === selectedCommand); });
      options[selectedCommand].scrollIntoView({ block: 'nearest' });
      return;
    }
    if (event.key === 'Escape') {
      event.preventDefault();
      if (captureCancel) {
        captureCancel();
        captureCancel = null;
        setStatus('已取消截图或拾取');
      } else if (!attachMenu.hidden) {
        showAttachMenu(false);
        attachToggle.focus();
      } else if (!commandsMenu.hidden) {
        showCommands(false);
      } else if (!contextMenu.hidden) {
        showContext(false);
        contextToggle.focus();
      } else if (!reasoning.hidden) {
        showReasoning(false);
        reasoningToggle.focus();
      } else if (!settings.hidden) {
        showSettings(false);
        control('settings-toggle').focus();
      } else if (!chats.hidden) {
        showChats(false);
        control('chats-toggle').focus();
      } else toggle(false);
    } else if (event.key === 'Enter' && !event.shiftKey && event.target === prompt) {
      event.preventDefault();
      if (event.repeat) return;
      if (!commandsMenu.hidden && selectedCommand >= 0) {
        commandsMenu.querySelectorAll('button:not(:disabled)')[selectedCommand]?.click();
      } else {
        control('composer').requestSubmit();
      }
    }
  });

  function addMessage(role, text, model = '', msgAttachments = []) {
    control('empty').hidden = true;
    const historyView = control('history');
    const item = append(historyView, 'article', '', `ai-message ai-${role}`);
    const heading = append(item, 'header');
    append(heading, 'strong', role === 'user' ? '你' : model || '模型');
    if (role === 'user') {
      renderMessageAttachments(item, msgAttachments, {
        onPreview: (att) => openAttachmentLightbox(att, { fetchMedia: fetchAttachmentMediaUrl }),
      });
      if (text) {
        if (text.length > 420) {
          append(item, 'p', `${text.slice(0, 160)}…\n…${text.slice(-100)}`, 'ai-request-preview');
          const original = append(item, 'details', '', 'ai-request-original');
          append(original, 'summary', '查看完整输入');
          append(original, 'p', text);
        } else {
          append(item, 'p', text);
        }
      }
      return item;
    }
    // 助手回复走富渲染：Markdown + ```ui 组件块；渲染失败就退回纯文本，别让一条回复毁掉整段对话。
    const content = append(item, 'div', '', 'ai-rich');
    try {
      renderRichReply(content, text);
    } catch (error) {
      console.warn('[AI 助手] 富回复渲染失败，已退回纯文本', error);
      content.className = 'ai-rich ai-rich-plain';
      content.textContent = text;
    }
    return item;
  }

  function showProgress(phases, started) {
    const nearLatest = body.scrollHeight - body.clientHeight - body.scrollTop < 40;
    const name = PHASES[phases.at(-1)] || PHASES.preparing;
    control('phase').textContent = name;
    control('elapsed').textContent = Math.floor((Date.now() - started) / 1000) + 's';
    control('progress').hidden = false;
    if (nearLatest) scrollToLatest();
  }

  function showTrace(message, phases, seconds) {
    if (!phases.length) return;
    const trace = append(message, 'details', '', 'ai-trace');
    message.querySelector('header').after(trace);
    append(trace, 'summary', `处理过程 · ${Math.max(1, seconds || 0)} 秒`);
    const list = append(trace, 'ol');
    for (const phase of phases) if (PHASES[phase]) append(list, 'li', PHASES[phase].replace('正在', ''));
  }

  function showEffects(message, effects) {
    if (!message || !effects?.length) return;
    const items = append(message, 'ul', '', 'ai-tool-effects');
    for (const effect of effects) append(items, 'li', `${effect.action} · ${effect.target}`);
  }

  function updateEffects(job, effects = []) {
    const pending = effects.slice(job.effects.length);
    job.effects = effects;
    if (pending.length) {
      try { window.top.dispatchEvent(new CustomEvent('macro-assistant-effects', { detail: pending })); }
      catch { /* 独立窗口会从磁盘读取工具写入的数据 */ }
    }
  }

  function showApproval(request) {
    if (approval?.id === request?.id) return;
    approval = request;
    control('approval').hidden = !request;
    if (request) {
      control('approval-action').textContent = request.action;
      control('approval-target').textContent = request.target || '';
      control('allow').textContent = '批准本次';
      control('deny').textContent = '拒绝';
      control('allow').disabled = false;
      control('deny').disabled = false;
    }
  }

  async function decide(allow) {
    if (!approval || !active?.id) return;
    control('allow').disabled = true;
    control('deny').disabled = true;
    try {
      await apiCall('assistant_approve', active.id, approval.id, allow);
      showApproval(null);
    } catch (error) {
      setStatus(error.message, true);
      control('allow').disabled = false;
      control('deny').disabled = false;
    }
  }
  control('allow').addEventListener('click', () => decide(true));
  control('deny').addEventListener('click', () => decide(false));

  function fillList(name, values) {
    const list = control(name);
    list.replaceChildren();
    for (const value of values) append(list, 'li', value);
    list.hidden = !values.length;
  }

  function showDraft(message, reply, before, attached, scope) {
    const proposal = editor.propose(reply, before, attached, scope);
    draft = { before, after: proposal.after, attached, scope, changed: proposal.diff.length > 0 };
    control('review-title').textContent = '查看修改草稿';
    control('review-actions').hidden = !draft.changed;
    control('target').textContent = editor.scopes.find(([key]) => key === scope)?.[1] || editor.title;
    control('stats').textContent = editor.stats(reply);
    fillList('approach', reply.approach || []);
    fillList('warnings', reply.warnings || []);
    const diff = control('diff');
    diff.replaceChildren();
    for (const line of proposal.diff.slice(0, 120)) append(diff, 'li', line.text, line.type ? `ai-diff-${line.type}` : '');
    if (proposal.diff.length > 120) append(diff, 'li', `另有 ${proposal.diff.length - 120} 项改动`);
    if (!proposal.diff.length) append(diff, 'li', '草稿与当前内容一致。');
    message.append(review);
    review.open = false;
    review.hidden = false;
    if (editor.autoApply && !draft.changed) {
      draft = null;
      setStatus('流程与当前画布一致');
      return;
    }
    if (editor.autoApply?.(before, scope)) {
      try {
        applyDraft();
      } catch (error) {
        review.open = true;
        setStatus(error.message, true);
      }
    } else {
      review.open = Boolean(editor.autoApply);
      setStatus(editor.autoApply ? '替换现有流程需要确认' : '草稿已校验');
    }
  }

  function clearDraft() {
    draft = null;
    review.hidden = true;
    review.open = false;
    control('history').after(review);
  }

  function restoreRequest(job) {
    if (!prompt.value && job.input) prompt.value = job.input;
    if (!attachments.length && job.attachments?.length) {
      attachments = [...job.attachments];
      renderAttachments();
      renderContext();
    }
    job.message?.remove();
    control('empty').hidden = Boolean(control('history').children.length);
  }

  control('composer').addEventListener('submit', (event) => {
    event.preventDefault();
    const query = commandQuery();
    if (query !== undefined) {
      showCommands(true, query);
      const options = [...commandsMenu.querySelectorAll('button:not(:disabled)')];
      if (!query) return;
      const match = options.find((option) => option.querySelector('code').textContent === '/' + query);
      const chosen = match || (options.length === 1 && commands.some(([name]) => name.startsWith(query)) ? options[0] : null);
      if (chosen && !commandsMenu.hidden) chosen.click();
      else if (commandsMenu.hidden) setStatus('没有这个命令。输入 /help 查看命令。', true);
      return;
    }
    submitPrompt(prompt.value);
  });

  async function submitPrompt(input, compacting = false) {
    const request = input.trim();
    const pendingAttachments = compacting ? [] : [...attachments];
    if ((!request && !pendingAttachments.length && !compacting) || active || chatsBusy || permissionBusy || recordingBusy) return;
    if (compacting && !chat) {
      setStatus('当前没有可压缩的对话。', true);
      return;
    }
    if (!modelSelect.value || settingsBusy) {
      setStatus('请先读取并选择一个模型。', true);
      return;
    }
    const effort = mode === 'off' ? 'off' : (level || 'auto');
    const model = modelSelect.value;
    const modelName = catalog[model]?.name || model;
    const imageCapable = catalog[model]?.image_input !== false;
    const attachCost = pendingAttachments.reduce((sum, att) => sum + estimateAttachmentTokens(att, imageCapable), 0);
    const job = { id: '', input, attachments: pendingAttachments, started: Date.now(), cancelled: false, phases: ['preparing'], effects: [],
      estimatedTokens: historyTokens + attachCost + estimateTokens(request ? JSON.stringify(request) : '') };
    active = job;
    showAttachMenu(false);
    showCommands(false);
    showContext(false);
    showReasoning(false);
    showSettings(false);
    showChats(false);
    setBusy();
    try {
      const before = compacting ? null : editor.snapshot();
      const attached = compacting ? null : editor.attach(before);
      if (!compacting) {
        job.message = addMessage('user', request, '', pendingAttachments);
        prompt.value = '';
        attachments = [];
        renderAttachments();
      }
      setStatus('');
      scrollToLatest();
      showProgress(job.phases, job.started);
      const result = await apiCall('assistant_start', editor.mode, compacting ? 'compact' : 'auto',
        compacting ? '/compact' : request, attached?.data ?? null, null, model, effort, chat, modelName, permission,
        pendingAttachments.length ? pendingAttachments : null);
      job.id = result.data.id;
      if (job.cancelled) {
        await apiCall('assistant_cancel', job.id);
        return;
      }
      chat = result.data.chat || chat;
      while (active === job) {
        const { data } = await apiCall('assistant_poll', job.id);
        if (active !== job) return;
        job.phases = data.phases;
        if (data.usage && data.usage.input_tokens > 0) {
          job.usage = data.usage;
          if (!compacting) lastUsage = { ...data.usage, model, chat };
          renderContext();
        }
        updateEffects(job, data.effects);
        showApproval(data.approval || null);
        showProgress(job.phases, job.started);
        if (data.state === 'finished') {
          if (!data.result.ok) throw new Error(data.result.error || '生成失败，请重试');
          const reply = data.result.data;
          if (compacting) {
            historyTokens = reply.context_tokens;
            lastUsage = null;
            setStatus('上下文已压缩，原对话仍可查看。');
            return;
          }
          historyTokens += attachCost + estimateTokens(request) + estimateTokens(reply.message);
          const message = addMessage('assistant', reply.message, modelName);
          showTrace(message, job.phases, Math.round((Date.now() - job.started) / 1000));
          showEffects(message, job.effects);
          if (reply.kind === 'draft') {
            showDraft(message, reply, before, attached, reply.scope || 'new');
          } else {
            setStatus(reply.kind === 'question' ? '补充信息后可继续' : '');
          }
          if (reply.history_error) setStatus(reply.history_error, true);
          control('progress').hidden = true;
          scrollToLatest();
          return;
        }
        if (data.state === 'cancelled') return;
        await wait(250);
      }
    } catch (error) {
      if (active !== job) return;
      if (job.effects.length) showEffects(job.message, job.effects);
      else restoreRequest(job);
      setStatus(error.message || '生成失败，请重试', true);
    } finally {
      if (active === job) {
        active = null;
        showApproval(null);
        control('progress').hidden = true;
        setBusy();
      }
    }
  }

  control('stop').addEventListener('click', () => {
    if (!active) return;
    const job = active;
    job.cancelled = true;
    active = null;
    if (job.effects.length) showEffects(job.message, job.effects);
    else restoreRequest(job);
    showApproval(null);
    control('progress').hidden = true;
    setStatus('已停止接收本轮结果');
    setBusy();
    if (job.id) apiCall('assistant_cancel', job.id).catch(() => {});
  });
  control('reset').addEventListener('click', () => loadChats(''));
  control('dismiss').addEventListener('click', () => {
    clearDraft();
    setStatus('已放弃草稿');
    prompt.focus({ preventScroll: true });
  });
  function applyDraft() {
    if (keyOf(editor.snapshot()) !== keyOf(draft.before) ||
        editor.sameSelection && !editor.sameSelection(draft.attached, draft.scope)) {
      throw new Error('编辑内容或选区已改变，请用当前内容重新生成。');
    }
    const before = draft.before;
    editor.apply(draft.after);
    previous = { before, applied: keyOf(editor.snapshot()) };
    control('undo').hidden = false;
    if (editor.autoApply) {
      draft = null;
      control('review-title').textContent = '已应用 · 查看流程改动';
      control('review-actions').hidden = true;
      review.open = !control('warnings').hidden;
      setStatus('流程已更新，可以保存或运行');
    } else {
      clearDraft();
      setStatus('已应用到编辑器');
    }
  }

  control('apply').addEventListener('click', () => {
    if (!draft || active) return;
    try {
      applyDraft();
      prompt.focus({ preventScroll: true });
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
      if (editor.autoApply && !draft && !review.hidden) {
        control('review-title').textContent = '已撤销 · 查看流程改动';
      }
      setStatus('已撤销 AI 修改');
    } catch (error) {
      setStatus(error.message, true);
    }
  });
  modelSelect.add(new Option('请先配置模型服务…', ''));
  setBusy();
  if (editor.defaultOpen) {
    toggle(true, false);
    const ready = () => {
      if (!panel.offsetWidth || !window.pywebview?.api) return;
      observer.disconnect();
      window.removeEventListener('automation-bridge-ready', ready);
      startAssistant();
    };
    const observer = new ResizeObserver(ready);
    observer.observe(panel);
    window.addEventListener('automation-bridge-ready', ready);
  }
  return toggle;
}
