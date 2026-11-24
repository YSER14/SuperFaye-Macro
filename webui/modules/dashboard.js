import { q } from './core/dom.js';
import { apiCall } from './core/api.js';

export async function refreshSnapshot() {
  try {
    const data = await apiCall('get_dashboard_snapshot');
    q('#statActive').textContent = String(data.active_macros || 0);

    const rec = data.recording || {};
    q('#statRec').textContent = rec.is_recording ? '录制中' : rec.is_armed ? '已布防' : '空闲';
    q('#statRecCount').textContent = `${rec.action_count || 0} / ${rec.track_count || 0}`;
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
    listenerButton.textContent = starting ? '正在启动全局监听…' : running ? '关闭全局宏监听' : '启动全局宏监听';
    listenerButton.disabled = starting;
    q('#statPaused').textContent = paused ? '是' : '否';
    const pauseButton = q('#btnGlobalPause');
    pauseButton.textContent = paused ? '解除全局暂停' : '全局暂停';
    pauseButton.setAttribute('aria-pressed', String(paused));

    // 「现有绑定」旁边那颗状态徽标：没开监听时热键按下去是完全没有反应的
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
