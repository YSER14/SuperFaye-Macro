// 枚举中文显示层
// ---------------------------------------------------------------------------
// 约定：宏文件、自动化脚本、webui_param_config.json 里存的**永远是英文原值**，
// 这里只负责「界面怎么显示」。任何下拉框、节点摘要、属性面板、参数面板都应该
// 走 enumLabel()/enumOptions()，不要直接把英文原值塞进 DOM。
//
// 查不到的值一律原样返回（未知枚举宁可露英文，也不能显示成空白）。

export const ENUM_LABELS = {
  // ── 坐标 / 鼠标 ─────────────────────────────────────────────
  screen: '屏幕',
  window: '窗口',
  relative: '相对',
  left: '左键',
  right: '右键',
  middle: '中键',
  click: '单击',
  dblclick: '双击',
  press: '按下',
  release: '松开',
  long_press: '长按',
  type: '输入',
  all: '全部',

  // ── 检测 ───────────────────────────────────────────────────
  contain: '包含',
  exact: '精确匹配',
  change: '变化',
  center: '中心点',
  mean: '平均值',
  max: '最大值',
  min: '最小值',
  max_diff: '最大差值',
  frame_diff: '帧差',
  time_interval: '固定间隔',
  disabled: '禁用',
  appear: '出现',
  disappear: '消失',
  bright: '变亮',
  frozen: '画面冻结',
  black: '黑屏',
  rapidocr: 'RapidOCR',
  regex: '正则匹配',
  regex_extract: '正则提取',
  number_gt: '数值大于',
  number_lt: '数值小于',
  number_eq: '数值等于',
  tm_ccoeff_normed: '归一化相关（CCOEFF）',
  tm_ccorr_normed: '归一化互相关（CCORR）',
  tm_sqdiff_normed: '归一化平方差（SQDIFF）',
  top_left: '左上',
  top_right: '右上',
  bottom_left: '左下',
  bottom_right: '右下',
  'top-left': '左上',
  'top-right': '右上',
  'bottom-left': '左下',
  'bottom-right': '右下',

  // ── 流程 / 控制 ────────────────────────────────────────────
  start: '开始',
  end: '结束',
  branch: '分支',
  default: '默认',
  none: '无',
  pause_task: '暂停任务',
  stop_task: '停止任务',
  stop_all: '停止全部',
  success: '成功',
  warning: '警告',
  error: '失败',
  info: '提示',

  // ── 数据类型 ────────────────────────────────────────────────
  number: '数值',
  string: '字符串',
  boolean: '布尔',
  text: '文本',
  true: '真',
  false: '假',

  // ── 运算 ───────────────────────────────────────────────────
  add: '相加',
  sub: '相减',
  mul: '相乘',
  div: '相除',
  mod: '取余',
  pow: '乘方',
  abs: '绝对值',
  sqrt: '平方根',
  ceil: '向上取整',
  floor: '向下取整',
  round: '四舍五入',
  negate: '取负',
  sin: '正弦',
  cos: '余弦',
  log: '对数',
  eq: '等于',
  ne: '不等于',
  gt: '大于',
  ge: '大于等于',
  lt: '小于',
  le: '小于等于',
  AND: '与',
  OR: '或',
  NOT: '非',
  and: '与',
  or: '或',
  not: '非',
  nand: '与非',
  nor: '或非',
  xor: '异或',

  // ── 字符串 ────────────────────────────────────────────────
  concat: '拼接',
  lower: '转小写',
  upper: '转大写',
  trim: '去首尾空白',
  replace: '替换',
  split: '分割',
  split_index: '分割后取项',
  substr: '截取（起点）',
  substring: '截取（区间）',

  // ── 运动曲线 / 时间线 ───────────────────────────────────────
  linear: '线性',
  ease_in: '缓入',
  ease_out: '缓出',
  ease_in_out: '缓入缓出',
  absolute: '绝对时间线',
  relative_compensated: '相对补偿时间线',
  final: '最终补偿',
  realtime: '实时补偿',

  // ── 录制 / 预设 ────────────────────────────────────────────
  delayed: '定时启动',
  first_event: '首个事件触发',
  fps: '射击（FPS）',
  desktop: '桌面',
  rpg3d: '3D 游戏',
  custom: '自定义',
};

// 值相同但语境不同的特例：节点类型 / 参数键 → 覆盖标签
export const ENUM_LABEL_OVERRIDES = {
  'action/key_action.action': { type: '键入' },
  'variable/set.temp_type': { number: '数值', string: '字符串', boolean: '布尔' },
};

// 空字符串在下拉框里的显示（快捷键留空、自定义留空等）
export const EMPTY_OPTION_LABEL = '（不设置）';

/**
 * 取枚举值的中文显示名。
 * @param {*} value 存的原始值
 * @param {string} [context] 可选语境键，命中 ENUM_LABEL_OVERRIDES 时优先
 * @returns {string}
 */
export function enumLabel(value, context) {
  if (value === null || value === undefined) return '';
  const raw = String(value);
  if (raw === '') return EMPTY_OPTION_LABEL;
  if (context && ENUM_LABEL_OVERRIDES[context]) {
    const override = ENUM_LABEL_OVERRIDES[context][raw];
    if (override) return override;
  }
  return Object.prototype.hasOwnProperty.call(ENUM_LABELS, raw) ? ENUM_LABELS[raw] : raw;
}

/**
 * 把选项列表转成 [{ value, label }]，供各下拉框统一渲染。
 * @param {Array<*>} options
 * @param {string} [context]
 */
export function enumOptions(options, context) {
  return (options || []).map((value) => ({ value, label: enumLabel(value, context) }));
}

/**
 * 渲染 <option> 列表（值保持英文原值，显示中文）。
 * @param {Array<*>} options
 * @param {*} current 当前值
 * @param {string} [context]
 */
export function enumOptionHtml(options, current, context) {
  const cur = String(current);
  return enumOptions(options, context)
    .map(({ value, label }) => {
      const raw = String(value);
      const selected = raw === cur ? ' selected' : '';
      return `<option value="${escapeAttr(raw)}"${selected}>${escapeHtml(label)}</option>`;
    })
    .join('');
}

function escapeHtml(text) {
  return String(text)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

function escapeAttr(text) {
  return escapeHtml(text);
}
