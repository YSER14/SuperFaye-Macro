import { q } from './core/dom.js';
import { apiCall } from './core/api.js';

export async function refreshSnapshot() {
  try {
    const data = await apiCall('get_dashboard_snapshot');
    const activeCount = Number(data.active_macros || 0);
    const statActive = q('#statActive');
    if (statActive) statActive.textContent = String(activeCount);
    const chipActive = q('#chipActive');
    if (chipActive) chipActive.classList.toggle('is-live', activeCount > 0);

    const rec = data.recording || {};
    const statRec = q('#statRec');
    if (statRec) {
      statRec.textContent = rec.is_recording ? '录制中' : rec.is_armed ? '已布防' : '空闲';
    }
    const chipRec = q('#chipRec');
    if (chipRec) {
      chipRec.classList.toggle('is-recording', Boolean(rec.is_recording));
      chipRec.classList.toggle('is-armed', Boolean(!rec.is_recording && rec.is_armed));
    }
    const statRecCount = q('#statRecCount');
    if (statRecCount) {
      statRecCount.textContent = `${rec.action_count || 0} / ${rec.track_count || 0}`;
    }
  } catch (err) {
    console.error(err);
  }
}

export async function refreshListenerButtonState() {
  try {
    const data = await apiCall('get_listener_state');
    const running = Boolean(data.running);
    const starting = Boolean(data.starting);
    const paused = Boolean(data.paused);
    const error = String(data.error || '');

    const listenerButton = q('#btnStartListeners');
    if (listenerButton) {
      listenerButton.textContent = starting ? '正在启动监听…' : running ? '关闭全局监听' : '启动全局监听';
      listenerButton.disabled = starting;
      listenerButton.classList.toggle('is-running', running && !paused);
    }

    const statusDot = q('#listenerStatusDot');
    const statusText = q('#listenerStatusText');
    if (statusDot) {
      statusDot.className = 'status-dot ' + (
        starting ? 'starting'
          : error && !running ? 'error'
            : running && paused ? 'paused'
              : running ? 'on'
                : 'off'
      );
    }
    if (statusText) {
      statusText.textContent = starting ? '正在启动监听…'
        : error && !running ? '监听启动异常'
          : running && paused ? '全局热键已暂停'
            : running ? '全局监听生效中'
              : '全局监听未启动';
    }

    const statPaused = q('#statPaused');
    if (statPaused) statPaused.textContent = paused ? '是' : '否';
    const chipPaused = q('#chipPaused');
    if (chipPaused) chipPaused.classList.toggle('is-paused', paused);

    const pauseButton = q('#btnGlobalPause');
    if (pauseButton) {
      pauseButton.textContent = paused ? '解除暂停' : '全局暂停';
      pauseButton.setAttribute('aria-pressed', String(paused));
      pauseButton.classList.toggle('is-paused', paused);
    }

    const badge = q('#bindingListenBadge');
    if (badge) {
      badge.disabled = starting;
      badge.classList.toggle('off', (!running && !error) || paused);
      badge.classList.toggle('on', running && !paused);
      badge.classList.toggle('error', Boolean(error) && !running);
      badge.classList.toggle('paused', running && paused);

      if (starting) {
        badge.textContent = '正在启动全局监听…';
        badge.title = '正在安装键盘与鼠标监听';
      } else if (error && !running) {
        badge.textContent = `监听启动失败 · ${error}`;
        badge.title = `全局监听没起来，按触发键不会有反应。错误：${error}`;
      } else if (running && paused) {
        badge.textContent = '热键已暂停 · 点此恢复';
        badge.title = '全局暂停已开启，点击恢复热键';
      } else if (running) {
        badge.textContent = '热键生效中';
        badge.title = '全局监听正在运行：按触发键会执行对应宏。点一下关闭监听。';
      } else {
        badge.textContent = '热键未生效 · 点此启动全局监听';
        badge.title = '全局监听还没启动，按物理按键不会有任何反应。点一下启动。';
      }
    }
  } catch (err) {
    console.error(err);
  }
}
