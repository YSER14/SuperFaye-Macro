/* AI 对话多模态输入辅助模块：图标、录音编码、视频抽帧、区域截图裁剪、附件预览与选择器 */

const MEDIA_ICONS = {
  plus: '<path d="M12 5v14"/><path d="M5 12h14"/>',
  mic: '<path d="M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3Z"/><path d="M19 10v2a7 7 0 0 1-14 0v-2"/><line x1="12" x2="12" y1="19" y2="22"/>',
  image: '<rect x="3" y="3" width="18" height="18" rx="2"/><circle cx="9" cy="9" r="2"/><path d="m21 15-3.086-3.086a2 2 0 0 0-2.828 0L6 21"/>',
  scissors: '<circle cx="6" cy="6" r="3"/><path d="M8.12 8.12 12 12"/><path d="M20 4 8.12 15.88"/><circle cx="6" cy="18" r="3"/><path d="M14.8 14.8 20 20"/>',
  timer: '<circle cx="12" cy="13" r="8"/><path d="M12 9v4l2 2"/><path d="M5 3 2 6"/><path d="m22 6-3-3"/><path d="M12 2v3"/>',
  window: '<rect x="2" y="4" width="20" height="16" rx="2"/><path d="M2 9h20"/><path d="M6 6.5h.01"/><path d="M9 6.5h.01"/>',
  video: '<rect x="2" y="5" width="14" height="14" rx="2"/><path d="m16 10 6-3v10l-6-3z"/>',
  audio: '<path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/>',
  file: '<path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8Z"/><path d="M14 2v6h6"/>',
  archive: '<path d="M21 8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16Z"/><path d="m3.3 7 8.7 5 8.7-5"/><path d="M12 22V12"/>',
  macro: '<path d="M13 2 4 14h7l-1 8 9-12h-7z"/>',
  macro_group: '<path d="m12 3 9 4.5-9 4.5-9-4.5Z"/><path d="m3 12 9 4.5 9-4.5"/><path d="m3 16.5 9 4.5 9-4.5"/>',
  auto_script: '<rect x="3" y="3" width="6" height="6" rx="1.5"/><rect x="15" y="3" width="6" height="6" rx="1.5"/><rect x="9" y="15" width="6" height="6" rx="1.5"/><path d="M6 9v3a2 2 0 0 0 2 2h8a2 2 0 0 0 2-2V9"/><path d="M12 14v1"/>',
  template: '<path d="m12 3-1.9 5.1L5 10l5.1 1.9L12 17l1.9-5.1L19 10l-5.1-1.9Z"/>',
  coord: '<circle cx="12" cy="12" r="8"/><path d="M12 2v4"/><path d="M12 18v4"/><path d="M2 12h4"/><path d="M18 12h4"/>',
  region: '<path d="M4 8V4h4"/><path d="M16 4h4v4"/><path d="M4 16v4h4"/><path d="M16 20h4v-4"/><rect x="8" y="8" width="8" height="8" rx="1"/>',
  url: '<path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71"/><path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71"/>',
  play: '<path d="m6 4 14 8-14 8z"/>',
  pause: '<rect x="6" y="4" width="4" height="16" rx="1"/><rect x="14" y="4" width="4" height="16" rx="1"/>',
  eye: '<path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7-10-7-10-7Z"/><circle cx="12" cy="12" r="3"/>',
  ocr: '<path d="M3 7V5a2 2 0 0 1 2-2h2"/><path d="M17 3h2a2 2 0 0 1 2 2v2"/><path d="M21 17v2a2 2 0 0 1-2 2h-2"/><path d="M7 21H5a2 2 0 0 1-2-2v-2"/><path d="M7 12h10"/>',
};

export function iconMarkup(name, size = 14) {
  const path = MEDIA_ICONS[name] || MEDIA_ICONS.file;
  return `<svg width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${path}</svg>`;
}

export function iconElement(name, size = 14) {
  const tpl = document.createElement('template');
  tpl.innerHTML = iconMarkup(name, size);
  return tpl.content.firstElementChild;
}

export function attachmentTypeIcon(kind) {
  switch (kind) {
    case 'image': return 'image';
    case 'audio': return 'audio';
    case 'video': return 'video';
    case 'archive': return 'archive';
    case 'macro': return 'macro';
    case 'macro_group': return 'macro_group';
    case 'auto_script': return 'auto_script';
    case 'template': return 'template';
    case 'coord': return 'coord';
    case 'region': return 'region';
    case 'window': return 'window';
    case 'url': return 'url';
    default: return 'file';
  }
}

export function formatDuration(ms) {
  const totalSec = Math.max(0, Math.round((ms || 0) / 1000));
  const m = Math.floor(totalSec / 60);
  const s = totalSec % 60;
  return `${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`;
}

export function estimateAttachmentTokens(att, imageCapable = true) {
  if (!att) return 0;
  let total = 32;
  if (att.text_excerpt) {
    total += Math.max(att.text_excerpt.length, Math.ceil(att.text_excerpt.length * 0.8));
  }
  if (att.transcript) {
    total += att.transcript.length + 16;
  }
  if (['image', 'template', 'region', 'window'].includes(att.kind) && att.path) {
    if (imageCapable && att.mode !== 'ocr') {
      const w = Math.min(att.width || 1024, 2000);
      const h = Math.min(att.height || 1024, 2000);
      total += Math.max(120, Math.min(1600, Math.round((w * h) / 750)));
    } else {
      total += 200;
    }
  } else if (att.kind === 'video') {
    const count = Math.min(att.frames?.length || 0, 8);
    total += count * (imageCapable && att.mode !== 'ocr' ? 900 : 160);
  } else if (att.kind === 'audio') {
    const secs = Math.round((att.duration_ms || 5000) / 1000);
    total += Math.max(80, Math.min(3200, secs * 32));
  }
  return total;
}

export function readFileAsDataUrl(file) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result || ''));
    reader.onerror = () => reject(new Error(`读取文件失败：${file.name || ''}`));
    reader.readAsDataURL(file);
  });
}

export async function extractVideoKeyframes(source, maxFrames = 6) {
  const isFile = source instanceof File || source instanceof Blob;
  const srcUrl = isFile ? URL.createObjectURL(source) : String(source || '');
  const video = document.createElement('video');
  video.muted = true;
  video.playsInline = true;
  video.preload = 'auto';

  try {
    await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('视频加载超时')), 12_000);
      video.onloadedmetadata = () => { clearTimeout(timer); resolve(); };
      video.onerror = () => { clearTimeout(timer); reject(new Error('无法解码视频格式')); };
      video.src = srcUrl;
    });

    const duration = Number.isFinite(video.duration) && video.duration > 0 ? video.duration : 0;
    const vw = video.videoWidth || 0;
    const vh = video.videoHeight || 0;
    if (!duration || !vw || !vh) throw new Error('未能读取视频画面或时长');

    const count = Math.min(maxFrames, Math.max(2, Math.ceil(duration / 2.5)));
    const canvas = document.createElement('canvas');
    const maxSide = 1280;
    const scale = Math.min(1, maxSide / Math.max(vw, vh));
    canvas.width = Math.max(1, Math.round(vw * scale));
    canvas.height = Math.max(1, Math.round(vh * scale));
    const ctx = canvas.getContext('2d');
    const frames = [];

    for (let i = 0; i < count; i += 1) {
      const ratio = count === 1 ? 0.5 : (0.04 + (0.92 * i) / (count - 1));
      const targetTime = Math.min(duration - 0.05, Math.max(0, duration * ratio));
      await new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error('视频帧读取超时')), 6000);
        const onSeeked = () => { clearTimeout(timer); resolve(); };
        video.addEventListener('seeked', onSeeked, { once: true });
        video.currentTime = targetTime;
      });
      ctx.drawImage(video, 0, 0, canvas.width, canvas.height);
      const dataUrl = canvas.toDataURL('image/jpeg', 0.84);
      frames.push({
        timestamp_ms: Math.round(targetTime * 1000),
        data_base64: dataUrl,
      });
    }

    return {
      duration_ms: Math.round(duration * 1000),
      width: vw,
      height: vh,
      frames,
    };
  } finally {
    video.removeAttribute('src');
    video.load();
    if (isFile) URL.revokeObjectURL(srcUrl);
  }
}

function downsampleAndEncodeWav(chunks, inputRate, targetRate = 16000) {
  let totalLen = 0;
  for (const c of chunks) totalLen += c.length;
  const merged = new Float32Array(totalLen);
  let offset = 0;
  for (const c of chunks) {
    merged.set(c, offset);
    offset += c.length;
  }

  let samples = merged;
  let rate = inputRate;
  if (inputRate > targetRate) {
    const ratio = inputRate / targetRate;
    const outLen = Math.max(1, Math.floor(merged.length / ratio));
    samples = new Float32Array(outLen);
    for (let i = 0; i < outLen; i += 1) {
      const start = Math.floor(i * ratio);
      const end = Math.min(merged.length, Math.floor((i + 1) * ratio));
      let sum = 0;
      for (let j = start; j < end; j += 1) sum += merged[j];
      samples[i] = sum / Math.max(1, end - start);
    }
    rate = targetRate;
  }

  const buffer = new ArrayBuffer(44 + samples.length * 2);
  const view = new DataView(buffer);
  const writeAscii = (pos, str) => {
    for (let i = 0; i < str.length; i += 1) view.setUint8(pos + i, str.charCodeAt(i));
  };
  writeAscii(0, 'RIFF');
  view.setUint32(4, 36 + samples.length * 2, true);
  writeAscii(8, 'WAVE');
  writeAscii(12, 'fmt ');
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, 1, true);
  view.setUint32(24, rate, true);
  view.setUint32(28, rate * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  writeAscii(36, 'data');
  view.setUint32(40, samples.length * 2, true);

  let pos = 44;
  for (let i = 0; i < samples.length; i += 1) {
    const s = Math.max(-1, Math.min(1, samples[i]));
    view.setInt16(pos, s < 0 ? s * 0x8000 : s * 0x7FFF, true);
    pos += 2;
  }
  return new Blob([buffer], { type: 'audio/wav' });
}

export function createVoiceRecorder({ onTick, onTranscript, onFinish, onError }) {
  let stream = null;
  let audioCtx = null;
  let processor = null;
  let sourceNode = null;
  let recognition = null;
  let timer = null;
  let startedAt = 0;
  let chunks = [];
  let transcriptText = '';
  let stopped = false;

  const cleanup = () => {
    if (timer) { clearInterval(timer); timer = null; }
    try { recognition?.stop(); } catch { /* 忽略 */ }
    recognition = null;
    try { processor?.disconnect(); } catch { /* 忽略 */ }
    try { sourceNode?.disconnect(); } catch { /* 忽略 */ }
    try { audioCtx?.close(); } catch { /* 忽略 */ }
    if (stream) {
      for (const track of stream.getTracks()) track.stop();
    }
    processor = null;
    sourceNode = null;
    audioCtx = null;
    stream = null;
  };

  return {
    async start() {
      if (!navigator.mediaDevices?.getUserMedia) {
        throw new Error('当前环境不支持麦克风录音，可直接上传音频文件');
      }
      stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      const AudioCtor = window.AudioContext || window.webkitAudioContext;
      audioCtx = new AudioCtor();
      sourceNode = audioCtx.createMediaStreamSource(stream);
      processor = audioCtx.createScriptProcessor(4096, 1, 1);
      chunks = [];
      transcriptText = '';
      stopped = false;
      startedAt = Date.now();

      processor.onaudioprocess = (event) => {
        if (stopped) return;
        const input = event.inputBuffer.getChannelData(0);
        chunks.push(new Float32Array(input));
      };
      sourceNode.connect(processor);
      processor.connect(audioCtx.destination);

      const SpeechRec = window.SpeechRecognition || window.webkitSpeechRecognition;
      if (SpeechRec) {
        try {
          recognition = new SpeechRec();
          recognition.lang = 'zh-CN';
          recognition.continuous = true;
          recognition.interimResults = true;
          let finalized = '';
          recognition.onresult = (evt) => {
            let interim = '';
            for (let i = evt.resultIndex; i < evt.results.length; i += 1) {
              const res = evt.results[i];
              const text = res[0]?.transcript || '';
              if (res.isFinal) finalized += text;
              else interim += text;
            }
            transcriptText = (finalized + interim).trim();
            onTranscript?.(transcriptText);
          };
          recognition.onerror = () => { /* 无网络或无系统语音服务时保持纯音频录制 */ };
          recognition.start();
        } catch {
          recognition = null;
        }
      }

      timer = setInterval(() => {
        const elapsed = Date.now() - startedAt;
        onTick?.(elapsed);
        if (elapsed >= 180_000) {
          this.stop();
        }
      }, 250);
      onTick?.(0);
    },

    async stop() {
      if (stopped) return;
      stopped = true;
      const durationMs = Math.max(200, Date.now() - startedAt);
      const sampleRate = audioCtx?.sampleRate || 44100;
      const recordedChunks = chunks;
      const finalTranscript = transcriptText.trim();
      cleanup();
      if (!recordedChunks.length) {
        onError?.(new Error('未录制到音频数据'));
        return;
      }
      const wavBlob = downsampleAndEncodeWav(recordedChunks, sampleRate, 16000);
      const dataUrl = await readFileAsDataUrl(wavBlob);
      onFinish?.({
        blob: wavBlob,
        data_base64: dataUrl,
        duration_ms: durationMs,
        transcript: finalTranscript,
      });
    },

    cancel() {
      if (stopped) return;
      stopped = true;
      cleanup();
    },
  };
}

export function openRegionCropper(capture) {
  return new Promise((resolve) => {
    const existing = document.getElementById('ai-capture-overlay');
    if (existing) existing.remove();

    const overlay = document.createElement('div');
    overlay.id = 'ai-capture-overlay';
    overlay.className = 'ai-capture-overlay';

    const topHint = document.createElement('div');
    topHint.className = 'ai-capture-hint';
    topHint.textContent = capture.title
      ? `${capture.title} — 拖动框选区域，或直接发送整张截图`
      : '在画面上拖动框选区域，或直接发送整张截图';

    const stage = document.createElement('div');
    stage.className = 'ai-capture-stage';

    const img = document.createElement('img');
    img.src = `data:image/png;base64,${capture.image}`;
    img.draggable = false;

    const canvas = document.createElement('canvas');
    stage.append(img, canvas);

    const bar = document.createElement('div');
    bar.className = 'ai-capture-bar';

    const coordsLabel = document.createElement('label');
    coordsLabel.className = 'ai-capture-check';
    const coordsCheck = document.createElement('input');
    coordsCheck.type = 'checkbox';
    coordsCheck.checked = true;
    const coordsText = document.createElement('span');
    coordsText.textContent = '附带屏幕坐标';
    coordsLabel.append(coordsCheck, coordsText);

    const statusBadge = document.createElement('span');
    statusBadge.className = 'ai-capture-status';
    statusBadge.textContent = `${capture.width}×${capture.height}`;

    const btnCancel = document.createElement('button');
    btnCancel.type = 'button';
    btnCancel.textContent = '取消 (Esc)';

    const btnFull = document.createElement('button');
    btnFull.type = 'button';
    btnFull.textContent = '发送整张截图';

    const btnCrop = document.createElement('button');
    btnCrop.type = 'button';
    btnCrop.className = 'ai-capture-primary';
    btnCrop.textContent = '确认选区';
    btnCrop.disabled = true;

    bar.append(coordsLabel, statusBadge, btnCancel, btnFull, btnCrop);
    overlay.append(topHint, stage, bar);
    document.body.append(overlay);

    let dragging = false;
    let startPt = null;
    let selection = null;

    const onMove = (e) => {
      if (!dragging || !startPt) return;
      const cur = toImgPoint(e.clientX, e.clientY);
      const x = Math.min(startPt.x, cur.x);
      const y = Math.min(startPt.y, cur.y);
      const w = Math.abs(cur.x - startPt.x);
      const h = Math.abs(cur.y - startPt.y);
      selection = { x, y, w, h };
      const ox = capture.origin_x || 0;
      const oy = capture.origin_y || 0;
      statusBadge.textContent = `选区 [${ox + x}, ${oy + y}, ${w}, ${h}]`;
      btnCrop.disabled = w < 4 || h < 4;
      syncCanvas();
    };

    const onUp = () => {
      if (!dragging) return;
      dragging = false;
      if (selection && (selection.w < 4 || selection.h < 4)) {
        selection = null;
        statusBadge.textContent = `${capture.width}×${capture.height}`;
        btnCrop.disabled = true;
        syncCanvas();
      }
    };

    const finish = (result) => {
      document.removeEventListener('keydown', onKey, true);
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
      window.removeEventListener('resize', syncCanvas);
      overlay.remove();
      resolve(result);
    };

    const onKey = (e) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        e.stopPropagation();
        finish(null);
      }
    };
    document.addEventListener('keydown', onKey, true);

    const syncCanvas = () => {
      const rect = img.getBoundingClientRect();
      canvas.width = Math.max(1, Math.round(rect.width));
      canvas.height = Math.max(1, Math.round(rect.height));
      canvas.style.width = `${rect.width}px`;
      canvas.style.height = `${rect.height}px`;
      const ctx = canvas.getContext('2d');
      ctx.clearRect(0, 0, canvas.width, canvas.height);
      if (!selection) return;
      const sx = canvas.width / capture.width;
      const sy = canvas.height / capture.height;
      const rx = selection.x * sx;
      const ry = selection.y * sy;
      const rw = selection.w * sx;
      const rh = selection.h * sy;
      ctx.fillStyle = 'rgba(0, 0, 0, 0.42)';
      ctx.fillRect(0, 0, canvas.width, canvas.height);
      ctx.clearRect(rx, ry, rw, rh);
      ctx.strokeStyle = '#22c55e';
      ctx.lineWidth = 2;
      ctx.strokeRect(rx, ry, rw, rh);
    };

    img.onload = syncCanvas;
    window.addEventListener('resize', syncCanvas);

    const toImgPoint = (clientX, clientY) => {
      const rect = img.getBoundingClientRect();
      const px = Math.max(0, Math.min(capture.width, ((clientX - rect.left) / Math.max(1, rect.width)) * capture.width));
      const py = Math.max(0, Math.min(capture.height, ((clientY - rect.top) / Math.max(1, rect.height)) * capture.height));
      return { x: Math.round(px), y: Math.round(py) };
    };

    img.addEventListener('mousedown', (e) => {
      e.preventDefault();
      dragging = true;
      startPt = toImgPoint(e.clientX, e.clientY);
      selection = { x: startPt.x, y: startPt.y, w: 0, h: 0 };
      btnCrop.disabled = true;
      syncCanvas();
    });

    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);

    btnCancel.addEventListener('click', () => finish(null));

    btnFull.addEventListener('click', () => {
      const ox = capture.origin_x || 0;
      const oy = capture.origin_y || 0;
      finish({
        kind: capture.hwnd ? 'window' : 'region',
        name: capture.title ? `窗口截图：${capture.title}` : `屏幕截图 (${capture.width}×${capture.height})`,
        data_base64: `data:image/png;base64,${capture.image}`,
        rect: coordsCheck.checked ? [ox, oy, capture.width, capture.height] : null,
        note: capture.title ? `窗口标题: ${capture.title}` : '',
      });
    });

    btnCrop.addEventListener('click', () => {
      if (!selection || selection.w < 4 || selection.h < 4) return;
      const outCanvas = document.createElement('canvas');
      outCanvas.width = selection.w;
      outCanvas.height = selection.h;
      const outCtx = outCanvas.getContext('2d');
      outCtx.drawImage(
        img,
        selection.x, selection.y, selection.w, selection.h,
        0, 0, selection.w, selection.h,
      );
      const ox = capture.origin_x || 0;
      const oy = capture.origin_y || 0;
      const absRect = [ox + selection.x, oy + selection.y, selection.w, selection.h];
      finish({
        kind: 'region',
        name: `区域截图 [${absRect.join(', ')}]`,
        data_base64: outCanvas.toDataURL('image/png'),
        rect: coordsCheck.checked ? absRect : null,
        note: capture.title ? `来自窗口: ${capture.title}` : '',
      });
    });
  });
}

export function openPickerModal({ title, placeholder = '搜索…', items = [], emptyText = '暂无可选项' }) {
  return new Promise((resolve) => {
    const existing = document.getElementById('ai-modal-backdrop');
    if (existing) existing.remove();

    const backdrop = document.createElement('div');
    backdrop.id = 'ai-modal-backdrop';
    backdrop.className = 'ai-modal-backdrop';

    const dialog = document.createElement('div');
    dialog.className = 'ai-modal';
    dialog.setAttribute('role', 'dialog');
    dialog.setAttribute('aria-modal', 'true');
    dialog.setAttribute('aria-label', title);

    const head = document.createElement('header');
    head.className = 'ai-modal-head';
    const h3 = document.createElement('h3');
    h3.textContent = title;
    const closeBtn = document.createElement('button');
    closeBtn.type = 'button';
    closeBtn.className = 'ai-quiet ai-icon';
    closeBtn.textContent = '×';
    head.append(h3, closeBtn);

    const search = document.createElement('input');
    search.type = 'search';
    search.className = 'ai-modal-search';
    search.placeholder = placeholder;
    search.autocomplete = 'off';

    const list = document.createElement('div');
    list.className = 'ai-modal-list';

    dialog.append(head, search, list);
    backdrop.append(dialog);
    document.body.append(backdrop);

    const finish = (val) => {
      document.removeEventListener('keydown', onKey, true);
      backdrop.remove();
      resolve(val);
    };

    const onKey = (e) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        e.stopPropagation();
        finish(null);
      }
    };
    document.addEventListener('keydown', onKey, true);
    backdrop.addEventListener('mousedown', (e) => {
      if (e.target === backdrop) finish(null);
    });
    closeBtn.addEventListener('click', () => finish(null));

    const renderList = () => {
      const q = search.value.trim().toLowerCase();
      list.replaceChildren();
      const filtered = items.filter((item) =>
        !q || String(item.label || '').toLowerCase().includes(q) || String(item.detail || '').toLowerCase().includes(q));
      if (!filtered.length) {
        const empty = document.createElement('p');
        empty.className = 'ai-muted';
        empty.textContent = items.length ? '没有匹配项' : emptyText;
        list.append(empty);
        return;
      }
      for (const item of filtered) {
        const btn = document.createElement('button');
        btn.type = 'button';
        btn.className = 'ai-modal-item';
        if (item.icon) btn.append(iconElement(item.icon, 14));
        const textWrap = document.createElement('div');
        textWrap.className = 'ai-modal-item-text';
        const strong = document.createElement('strong');
        strong.textContent = item.label;
        textWrap.append(strong);
        if (item.detail) {
          const span = document.createElement('span');
          span.textContent = item.detail;
          textWrap.append(span);
        }
        btn.append(textWrap);
        if (item.badge) {
          const badge = document.createElement('code');
          badge.className = 'ai-modal-badge';
          badge.textContent = item.badge;
          btn.append(badge);
        }
        btn.addEventListener('click', () => finish(item));
        list.append(btn);
      }
    };

    search.addEventListener('input', renderList);
    search.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') {
        e.preventDefault();
        list.querySelector('button.ai-modal-item')?.click();
      }
    });
    renderList();
    requestAnimationFrame(() => search.focus());
  });
}

export function openUrlInputModal() {
  return new Promise((resolve) => {
    const existing = document.getElementById('ai-modal-backdrop');
    if (existing) existing.remove();

    const backdrop = document.createElement('div');
    backdrop.id = 'ai-modal-backdrop';
    backdrop.className = 'ai-modal-backdrop';

    const dialog = document.createElement('form');
    dialog.className = 'ai-modal ai-modal-compact';
    dialog.innerHTML = `
      <header class="ai-modal-head">
        <h3>添加网页链接</h3>
        <button type="button" class="ai-quiet ai-icon" data-close="1">×</button>
      </header>
      <div class="ai-modal-form">
        <label>公开网址（HTTP / HTTPS）
          <input type="url" data-field="url" required maxlength="2048" placeholder="https://example.com/guide" />
        </label>
        <label>显示名称（可选）
          <input type="text" data-field="name" maxlength="80" placeholder="例如：角色手法说明" />
        </label>
      </div>
      <div class="ai-modal-foot">
        <button type="button" data-close="1">取消</button>
        <button type="submit" class="ai-primary">添加到对话</button>
      </div>`;

    backdrop.append(dialog);
    document.body.append(backdrop);

    const urlInput = dialog.querySelector('[data-field="url"]');
    const nameInput = dialog.querySelector('[data-field="name"]');

    const finish = (val) => {
      document.removeEventListener('keydown', onKey, true);
      backdrop.remove();
      resolve(val);
    };
    const onKey = (e) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        e.stopPropagation();
        finish(null);
      }
    };
    document.addEventListener('keydown', onKey, true);
    backdrop.addEventListener('mousedown', (e) => {
      if (e.target === backdrop) finish(null);
    });
    for (const btn of dialog.querySelectorAll('[data-close]')) {
      btn.addEventListener('click', () => finish(null));
    }
    dialog.addEventListener('submit', (e) => {
      e.preventDefault();
      const url = urlInput.value.trim();
      if (!url) return;
      finish({ url, name: nameInput.value.trim() });
    });
    requestAnimationFrame(() => urlInput.focus());
  });
}

export async function openAttachmentLightbox(att, { fetchMedia, onToggleMode, editable = false } = {}) {
  const existing = document.getElementById('ai-modal-backdrop');
  if (existing) existing.remove();

  const backdrop = document.createElement('div');
  backdrop.id = 'ai-modal-backdrop';
  backdrop.className = 'ai-modal-backdrop';

  const dialog = document.createElement('div');
  dialog.className = 'ai-modal ai-modal-wide';

  const head = document.createElement('header');
  head.className = 'ai-modal-head';
  const titleWrap = document.createElement('div');
  titleWrap.className = 'ai-modal-title-wrap';
  const h3 = document.createElement('h3');
  h3.textContent = att.name || '附件预览';
  const sub = document.createElement('span');
  sub.className = 'ai-muted';
  sub.textContent = att.summary || att.url || att.path || '';
  titleWrap.append(h3, sub);

  const headActions = document.createElement('div');
  headActions.className = 'ai-modal-head-actions';
  if (editable && onToggleMode && ['image', 'template', 'region', 'window', 'video'].includes(att.kind)) {
    const modeBtn = document.createElement('button');
    modeBtn.type = 'button';
    const syncLabel = () => {
      modeBtn.textContent = att.mode === 'ocr' ? '当前：仅本地 OCR（点击改回识图）' : '当前：模型识图（点击改本地 OCR）';
    };
    syncLabel();
    modeBtn.addEventListener('click', () => {
      onToggleMode(att);
      syncLabel();
    });
    headActions.append(modeBtn);
  }
  const closeBtn = document.createElement('button');
  closeBtn.type = 'button';
  closeBtn.className = 'ai-quiet ai-icon';
  closeBtn.textContent = '×';
  headActions.append(closeBtn);
  head.append(titleWrap, headActions);

  const body = document.createElement('div');
  body.className = 'ai-modal-preview';

  dialog.append(head, body);
  backdrop.append(dialog);
  document.body.append(backdrop);

  const finish = () => {
    document.removeEventListener('keydown', onKey, true);
    backdrop.remove();
  };
  const onKey = (e) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      finish();
    }
  };
  document.addEventListener('keydown', onKey, true);
  backdrop.addEventListener('mousedown', (e) => {
    if (e.target === backdrop) finish();
  });
  closeBtn.addEventListener('click', finish);

  if (['image', 'template', 'region', 'window'].includes(att.kind) && (att.preview_url || att.path)) {
    const img = document.createElement('img');
    img.className = 'ai-lightbox-img';
    img.src = att.preview_url || '';
    img.alt = att.name || '图片';
    body.append(img);
    if (att.path && fetchMedia) {
      fetchMedia(att.path).then((url) => { if (url) img.src = url; }).catch(() => {});
    }
  } else if (att.kind === 'video' && att.frames?.length) {
    const grid = document.createElement('div');
    grid.className = 'ai-video-frame-grid';
    for (const frame of att.frames) {
      const fig = document.createElement('figure');
      const img = document.createElement('img');
      img.src = frame.preview_url || '';
      img.alt = formatDuration(frame.timestamp_ms);
      const cap = document.createElement('figcaption');
      cap.textContent = `${(frame.timestamp_ms / 1000).toFixed(1)}s`;
      fig.append(img, cap);
      grid.append(fig);
      if (frame.path && fetchMedia) {
        fetchMedia(frame.path).then((url) => { if (url) img.src = url; }).catch(() => {});
      }
    }
    body.append(grid);
  } else if (att.kind === 'audio') {
    const wrap = document.createElement('div');
    wrap.className = 'ai-audio-preview';
    const audio = document.createElement('audio');
    audio.controls = true;
    wrap.append(audio);
    if (att.transcript) {
      const p = document.createElement('p');
      p.textContent = `语音内容：${att.transcript}`;
      wrap.append(p);
    }
    body.append(wrap);
    if (att.local_url) {
      audio.src = att.local_url;
    } else if (att.path && fetchMedia) {
      fetchMedia(att.path).then((url) => { if (url) audio.src = url; }).catch(() => {});
    }
  } else {
    const pre = document.createElement('pre');
    pre.className = 'ai-lightbox-text';
    pre.textContent = att.text_excerpt || att.url || att.summary || att.path || '无预览文本';
    body.append(pre);
  }
}

export function renderComposerAttachments(container, attachments, {
  capability = {},
  onRemove,
  onToggleMode,
  onPreview,
} = {}) {
  container.replaceChildren();
  container.hidden = !attachments.length;
  if (!attachments.length) return;

  const list = document.createElement('div');
  list.className = 'ai-attach-list';

  let needsOcrNotice = false;
  let needsAudioNotice = false;

  for (const att of attachments) {
    const isVisual = ['image', 'template', 'region', 'window'].includes(att.kind) && Boolean(att.path);
    const isVideo = att.kind === 'video';
    if ((isVisual || isVideo) && capability.image_input === false && att.mode !== 'ocr') {
      needsOcrNotice = true;
    }
    if (att.kind === 'audio' && capability.audio_input === false && !att.transcript) {
      needsAudioNotice = true;
    }

    if ((isVisual || isVideo) && att.preview_url) {
      const card = document.createElement('div');
      card.className = 'ai-attach-thumb';
      card.title = `${att.name}${att.summary ? ' · ' + att.summary : ''}`;

      const thumbBtn = document.createElement('button');
      thumbBtn.type = 'button';
      thumbBtn.className = 'ai-attach-thumb-btn';
      const img = document.createElement('img');
      img.src = att.preview_url;
      img.alt = att.name;
      thumbBtn.append(img);
      thumbBtn.addEventListener('click', () => onPreview?.(att));

      const badge = document.createElement('button');
      badge.type = 'button';
      badge.className = 'ai-attach-mode';
      const ocrOnly = att.mode === 'ocr' || capability.image_input === false;
      badge.textContent = isVideo
        ? (att.summary || `${att.frames?.length || 0}帧`)
        : ocrOnly ? 'OCR' : att.rect ? '坐标图' : '识图';
      badge.title = isVideo ? '点击查看提取的关键帧' : '点击切换「模型识图 / 仅本地 OCR」';
      badge.addEventListener('click', (e) => {
        e.stopPropagation();
        if (isVideo) onPreview?.(att);
        else onToggleMode?.(att);
      });

      const removeBtn = document.createElement('button');
      removeBtn.type = 'button';
      removeBtn.className = 'ai-attach-remove';
      removeBtn.setAttribute('aria-label', `移除 ${att.name}`);
      removeBtn.textContent = '×';
      removeBtn.addEventListener('click', (e) => {
        e.stopPropagation();
        onRemove?.(att.id);
      });

      card.append(thumbBtn, badge, removeBtn);
      list.append(card);
      continue;
    }

    const chip = document.createElement('div');
    chip.className = 'ai-attach-chip';
    chip.title = `${att.name}${att.summary ? ' · ' + att.summary : ''}${att.path ? '\n' + att.path : ''}`;

    const bodyBtn = document.createElement('button');
    bodyBtn.type = 'button';
    bodyBtn.className = 'ai-attach-chip-btn';
    bodyBtn.append(iconElement(attachmentTypeIcon(att.kind), 13));

    const label = document.createElement('span');
    label.className = 'ai-attach-name';
    label.textContent = att.name;
    bodyBtn.append(label);

    if (att.summary) {
      const meta = document.createElement('small');
      meta.className = 'ai-attach-meta';
      meta.textContent = att.summary;
      bodyBtn.append(meta);
    }
    bodyBtn.addEventListener('click', () => onPreview?.(att));

    const removeBtn = document.createElement('button');
    removeBtn.type = 'button';
    removeBtn.className = 'ai-attach-chip-del';
    removeBtn.setAttribute('aria-label', `移除 ${att.name}`);
    removeBtn.textContent = '×';
    removeBtn.addEventListener('click', () => onRemove?.(att.id));

    chip.append(bodyBtn, removeBtn);
    list.append(chip);
  }

  container.append(list);

  if (needsOcrNotice || needsAudioNotice) {
    const note = document.createElement('p');
    note.className = 'ai-attach-notice';
    const msgs = [];
    if (needsOcrNotice) msgs.push('当前模型为纯文本，图片/视频帧将自动转为本地 OCR 文字');
    if (needsAudioNotice) msgs.push('当前模型目录未声明支持音频输入');
    note.textContent = msgs.join('；');
    container.append(note);
  }
}

export function renderMessageAttachments(parent, attachments, { onPreview } = {}) {
  if (!Array.isArray(attachments) || !attachments.length) return;
  const wrap = document.createElement('div');
  wrap.className = 'ai-msg-attachments';
  for (const att of attachments) {
    if (['image', 'template', 'region', 'window', 'video'].includes(att.kind) && att.preview_url) {
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'ai-msg-thumb';
      btn.title = `${att.name}${att.summary ? ' · ' + att.summary : ''}`;
      const img = document.createElement('img');
      img.src = att.preview_url;
      img.alt = att.name || '图片';
      btn.append(img);
      if (att.kind === 'video' || att.rect) {
        const tag = document.createElement('span');
        tag.className = 'ai-msg-thumb-tag';
        tag.textContent = att.kind === 'video' ? `${att.frames?.length || 0}帧` : '区域';
        btn.append(tag);
      }
      btn.addEventListener('click', () => onPreview?.(att));
      wrap.append(btn);
      continue;
    }

    const chip = document.createElement('button');
    chip.type = 'button';
    chip.className = 'ai-msg-chip';
    chip.title = att.summary || att.name || '';
    chip.append(iconElement(attachmentTypeIcon(att.kind), 12));
    const name = document.createElement('span');
    name.textContent = att.name || '附件';
    chip.append(name);
    if (att.summary) {
      const meta = document.createElement('small');
      meta.textContent = att.summary;
      chip.append(meta);
    }
    chip.addEventListener('click', () => onPreview?.(att));
    wrap.append(chip);
  }
  parent.append(wrap);
}
