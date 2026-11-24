/**
 * 拖拽贴边自动滚动
 *
 * 为什么自己写：Chrome（无头）和 Edge WebView2 的原生「拖到边缘自动滚」在
 * `draggable` 行 + 自定义 dragover 处理下经常完全不触发；长动作列表里想把
 * 一块拖到看不见的位置，只能松手、滚一下、再抓起来。
 *
 * 做法：在 document 捕获阶段接住 dragover / dragenter，记下指针位置，用 rAF
 * 逐帧按「离上/下缘多近」算速度滚动容器；贴着边缘不动就一直滚。
 *
 * 三个坑（都在这处理了）：
 * 1. 拖拽静止时浏览器不再补 dragover，所以坐标只在移动时更新一次也没关系，
 *    循环会一直跑；快速拖动时 Chromium 往往只发 dragenter 不补最后的 dragover，
 *    所以 dragenter 也要听，否则坐标永远停在起手位置。
 * 2. 指针不动、内容在滚，浏览器不会重新做命中测试：悬停高亮会黏在滚走的那
 *    一行上。所以滚动时自己用 elementFromPoint 补一次 dragenter / dragover。
 * 3. 更要命的是：只要内容在指针底下被动滚过，Chromium 松手时会认为落点无效
 *    —— 只发 dragend，不发 drop，动作就白拖了。所以「这次拖拽真的滚过、又没
 *    收到 drop」时，在 dragend 里按指针位置补一次合成 drop（晚 160ms 再动手：
 *    Esc 取消拖拽时页面收不到 keydown，但 dragend 之后会收到 keyup:Escape，
 *    抢在那之前取消掉这次补投）。
 *
 * 用法：attachDragAutoScroll(el, { edge, minSpeed, maxSpeed })
 * 返回 detach()；对同一元素重复 attach 会复用同一个实例。
 */

const DEFAULT_EDGE = 84;        // 离上下缘多少像素内开始滚
const DEFAULT_MIN_SPEED = 1.4;  // 刚进贴边区时每帧滚动像素（≈84px/s）
const DEFAULT_MAX_SPEED = 22;   // 紧贴边缘时每帧滚动像素（≈1300px/s）
const HIT_TEST_INTERVAL = 70;   // 自动滚动时补高亮的最小间隔（ms）
const GHOST_DROP_DELAY = 160;   // 松手到「补 drop」之间的观察窗，用来收 keyup:Escape

export function attachDragAutoScroll(container, options = {}) {
  if (!(container instanceof Element)) return () => {};
  if (container.__dragAutoScrollDetach) return container.__dragAutoScrollDetach;

  const edge = Number.isFinite(options.edge) ? options.edge : DEFAULT_EDGE;
  const minSpeed = Number.isFinite(options.minSpeed) ? options.minSpeed : DEFAULT_MIN_SPEED;
  const maxSpeed = Number.isFinite(options.maxSpeed) ? options.maxSpeed : DEFAULT_MAX_SPEED;

  let rafId = 0;
  let pointerX = NaN;
  let pointerY = NaN;
  let direction = 0;
  let hoverRow = null;
  let lastHitTestAt = 0;
  let lastDataTransfer = null;
  let dragData = [];
  // 一次拖拽的生命周期状态：滚过没有 / 浏览器给没给 drop / 是不是被 Esc 取消的
  let dragActive = false;
  let didScroll = false;
  let sawDrop = false;
  let cancelledByEscape = false;
  let ghostTimer = 0;

  const mark = (dir) => {
    if (dir === direction) return;
    direction = dir;
    if (dir) {
      container.dataset.dragScrolling = dir < 0 ? 'up' : 'down';
    } else {
      delete container.dataset.dragScrolling;
    }
  };

  const resetDrag = () => {
    if (rafId) {
      cancelAnimationFrame(rafId);
      rafId = 0;
    }
    pointerX = NaN;
    pointerY = NaN;
    hoverRow = null;
    lastDataTransfer = null;
    dragData = [];
    dragActive = false;
    didScroll = false;
    sawDrop = false;
    cancelledByEscape = false;
    mark(0);
  };

  /** 指针下面那一行（行是 li；指针在容器外或落在内边距上就是 null）。 */
  const rowAt = (x, y) => {
    if (!Number.isFinite(x) || !Number.isFinite(y)) return null;
    const el = document.elementFromPoint(x, y);
    if (!(el instanceof Element) || !container.contains(el)) return null;
    return el.closest('li');
  };

  /** 内容在滚、指针没动：把 dragenter / dragover / dragleave 补给新的那一行，
   *  让落点高亮跟着手指走，而不是黏在滚走的那一行上。 */
  const syncHover = (now) => {
    if (now - lastHitTestAt < HIT_TEST_INTERVAL) return;
    lastHitTestAt = now;
    const row = rowAt(pointerX, pointerY);
    if (row === hoverRow) return;

    const init = { bubbles: true, cancelable: true, dataTransfer: lastDataTransfer };
    if (hoverRow && hoverRow.isConnected) {
      hoverRow.dispatchEvent(new DragEvent('dragleave', init));
    }
    hoverRow = row;
    if (row) {
      row.dispatchEvent(new DragEvent('dragenter', init));
      row.dispatchEvent(new DragEvent('dragover', init));
    }
  };

  /** 离上下缘越近越快；指针已经越过边缘（拖出可见区一点）就是顶速。 */
  const speedFor = (distance) => {
    const clamped = Math.max(0, Math.min(distance, edge));
    const closeness = 1 - clamped / edge;
    return minSpeed + closeness * closeness * (maxSpeed - minSpeed);
  };

  /** 这一帧该滚多少像素：负数＝往上，正数＝往下，0＝不滚。 */
  const frameDelta = () => {
    if (!Number.isFinite(pointerY) || !container.isConnected) return 0;
    const rect = container.getBoundingClientRect();
    if (!rect.height) return 0;
    // 容器可能比窗口还高：滚动只发生在「看得见的那一段」边缘
    const viewTop = Math.max(rect.top, 0);
    const viewBottom = Math.min(rect.bottom, window.innerHeight);
    if (viewBottom - viewTop <= 0) return 0;
    // 横向拖到别的栏里就别自作主张滚动作列表了
    if (pointerX < rect.left - 48 || pointerX > rect.right + 48) return 0;
    if (pointerY < viewTop - edge || pointerY > viewBottom + edge) return 0;

    const fromTop = pointerY - viewTop;
    const fromBottom = viewBottom - pointerY;
    if (fromTop < edge) return -speedFor(fromTop);
    if (fromBottom < edge) return speedFor(fromBottom);
    return 0;
  };

  const tick = (now) => {
    rafId = 0;
    if (!Number.isFinite(pointerY)) return;
    const delta = frameDelta();
    if (delta) {
      const before = container.scrollTop;
      container.scrollTop = before + delta;
      const moved = container.scrollTop !== before;
      mark(moved ? (container.scrollTop < before ? -1 : 1) : 0);
      if (moved) {
        didScroll = true;
        syncHover(now);
      }
    } else {
      mark(0);
    }
    rafId = requestAnimationFrame(tick);
  };

  const onDragEnterOrOver = (event) => {
    // 只认浏览器自己发的拖放事件；syncHover 补的合成事件坐标是 0，收进来会带偏指针
    if (!event.isTrusted) return;
    if (!dragActive) {
      dragActive = true;
      didScroll = false;
      sawDrop = false;
      cancelledByEscape = false;
    }
    pointerX = event.clientX;
    pointerY = event.clientY;
    lastDataTransfer = event.dataTransfer;
    if (!rafId) rafId = requestAnimationFrame(tick);
  };

  const onDragStart = (event) => {
    if (!event.isTrusted) return;
    cancelGhost();
    dragActive = true;
    didScroll = false;
    sawDrop = false;
    cancelledByEscape = false;
    dragData = [];
  };

  /** dragstart 的冒泡阶段才读得到数据：源元素是在自己的 dragstart 监听里
   *  setData 的，捕获阶段跑在它前面；而且 dragover / dragenter 期间
   *  dataTransfer 处于保护模式，getData 一律返回空。 */
  const onDragStartSnapshot = (event) => {
    if (!event.isTrusted) return;
    dragData = snapshotData(event.dataTransfer);
  };

  const onKeyDown = (event) => {
    if (dragActive && event.key === 'Escape') cancelledByEscape = true;
  };

  /** 把 dataTransfer 的内容抄成 [{type, value}]；读不到就是空。 */
  const snapshotData = (source) => {
    const items = [];
    if (!source) return items;
    try {
      for (const type of Array.from(source.types || [])) {
        const value = source.getData(type);
        if (value) items.push([type, value]);
      }
    } catch (err) {
      /* 读不到就算了，后面还有 state.draggedActionPaths 兜底 */
    }
    return items;
  };

  /** 用抄好的内容造一个新的 dataTransfer，给合成 drop 用。 */
  const rebuildDataTransfer = (source) => {
    const copy = new DataTransfer();
    const items = dragData.length ? dragData : snapshotData(source);
    try {
      for (const [type, value] of items) copy.setData(type, value);
    } catch (err) {
      /* 空壳也能用：动作移动靠 state.draggedActionPaths 兜底 */
    }
    return copy;
  };

  const dispatchDrop = (row, x, y, dataTransfer) => {
    let synthetic;
    try {
      synthetic = new DragEvent('drop', {
        bubbles: true,
        cancelable: true,
        clientX: x,
        clientY: y,
        dataTransfer,
      });
    } catch (err) {
      return false;
    }
    row.dispatchEvent(synthetic);
    return true;
  };

  /** 松手时补一次命中测试：指针没动、内容滚过，浏览器给的落点可能是滚走前那一行。 */
  const retargetDrop = (event) => {
    const target = event.target instanceof Element ? event.target : null;
    const eventRow = target ? target.closest('li') : null;
    const row = rowAt(event.clientX, event.clientY);
    if (!row || row === eventRow) return;
    // 先造事件再吞掉原始 drop：造不出来就按浏览器原来的落点走，别把这次拖放丢了
    if (!dispatchDrop(row, event.clientX, event.clientY, event.dataTransfer)) return;
    event.preventDefault();
    event.stopPropagation();
  };

  const onDrop = (event) => {
    if (!event.isTrusted) return;
    sawDrop = true;
    retargetDrop(event);
    resetDrag();
  };

  const onDragEnd = (event) => {
    if (!event.isTrusted) return;
    // 内容在指针底下滚过 → 浏览器把这次 drop 判成无效，只发 dragend。
    // 用户松手时想落的那一行就是指针现在压着的行，按它补一次 drop。
    if (dragActive && didScroll && !sawDrop && !cancelledByEscape) {
      const row = rowAt(event.clientX, event.clientY);
      if (row) {
        const x = event.clientX;
        const y = event.clientY;
        const dataTransfer = rebuildDataTransfer(event.dataTransfer);
        // 晚一帧多再补：Esc 取消拖拽时页面收不到 keydown，但会在 dragend 之后
        // 十几毫秒收到 keyup:Escape，抢在那之前动手就能把「取消」认回来
        cancelGhost();
        ghostTimer = setTimeout(() => {
          ghostTimer = 0;
          if (row.isConnected) dispatchDrop(row, x, y, dataTransfer);
        }, GHOST_DROP_DELAY);
      }
    }
    resetDrag();
  };

  const cancelGhost = () => {
    if (!ghostTimer) return;
    clearTimeout(ghostTimer);
    ghostTimer = 0;
  };

  const onEscape = (event) => {
    if (event.key === 'Escape') cancelGhost();
  };

  const onWindowBlur = () => resetDrag();
  const onVisibilityChange = () => {
    if (document.hidden) resetDrag();
  };

  document.addEventListener('dragstart', onDragStart, true);
  document.addEventListener('dragstart', onDragStartSnapshot);
  document.addEventListener('dragover', onDragEnterOrOver, true);
  document.addEventListener('dragenter', onDragEnterOrOver, true);
  document.addEventListener('drop', onDrop, true);
  document.addEventListener('dragend', onDragEnd, true);
  document.addEventListener('keydown', onKeyDown, true);
  document.addEventListener('keydown', onEscape, true);
  document.addEventListener('keyup', onEscape, true);
  window.addEventListener('blur', onWindowBlur);
  document.addEventListener('visibilitychange', onVisibilityChange);

  const detach = () => {
    cancelGhost();
    resetDrag();
    document.removeEventListener('dragstart', onDragStart, true);
    document.removeEventListener('dragstart', onDragStartSnapshot);
    document.removeEventListener('dragover', onDragEnterOrOver, true);
    document.removeEventListener('dragenter', onDragEnterOrOver, true);
    document.removeEventListener('drop', onDrop, true);
    document.removeEventListener('dragend', onDragEnd, true);
    document.removeEventListener('keydown', onKeyDown, true);
    document.removeEventListener('keydown', onEscape, true);
    document.removeEventListener('keyup', onEscape, true);
    window.removeEventListener('blur', onWindowBlur);
    document.removeEventListener('visibilitychange', onVisibilityChange);
    delete container.__dragAutoScrollDetach;
  };

  container.__dragAutoScrollDetach = detach;
  return detach;
}
