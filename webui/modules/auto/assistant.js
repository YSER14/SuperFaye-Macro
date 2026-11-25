import { mountAssistant } from '../assistant-panel.js';

const linkKey = (link) => `#${link.from.node}.${link.from.port} → #${link.to.node}.${link.to.port}`;
const formatValue = (value) => value === undefined ? '默认' : Array.isArray(value)
  ? '[' + value.join(', ') + ']' : typeof value === 'string' ? value : JSON.stringify(value);
const emptyGraph = { nodes: [], connections: [], annotations: [], groups: [] };

function nodeDiff(diff, before, after, labels) {
  const previous = new Map(before.map((entry) => [entry.id, entry]));
  const next = new Map(after.map((entry) => [entry.id, entry]));
  const label = (node) => `#${node.id} ${node.customName || labels.node(node.type)}`;
  for (const [id, node] of previous) {
    if (!next.has(id)) diff.push({ type: 'remove', text: `− ${label(node)}` });
  }
  for (const [id, node] of next) {
    const old = previous.get(id);
    if (!old) {
      const values = Object.entries(node.params).map(([key, value]) =>
        `${labels.param(node.type, key)} ${formatValue(value)}`);
      const params = values.length ? '\n' + values.join(' · ') : '';
      diff.push({ type: 'add', text: `+ ${label(node)}${params}` });
      continue;
    }
    if (old.type !== node.type || old.customName !== node.customName) {
      diff.push({ text: `${label(old)} → ${label(node)}` });
    }
    if (old.position.x !== node.position.x || old.position.y !== node.position.y) {
      diff.push({ text: `${label(node)} · 移动到 (${node.position.x}, ${node.position.y})` });
    }
    const params = new Set([...Object.keys(old.params), ...Object.keys(node.params)]);
    for (const key of params) {
      const left = old.params[key];
      const right = node.params[key];
      if (JSON.stringify(left) !== JSON.stringify(right)) {
        diff.push({ text: `${label(node)} · ${labels.param(node.type, key)} ${formatValue(left)} → ${formatValue(right)}` });
      }
    }
  }
}

function describeLink(link, graph, labels) {
  const describe = ({ node: id, port }) => {
    const node = graph.nodes.find((item) => item.id === id);
    const name = node ? (node.customName || labels.node(node.type)) : '';
    return [`#${id}`, name, labels.port(port)].filter(Boolean).join(' ');
  };
  return `${describe(link.from)} → ${describe(link.to)}`;
}

function graphDiff(before, after, labels) {
  const diff = [];
  nodeDiff(diff, before.nodes, after.nodes, labels);
  const beforeLinks = new Map(before.connections.map((link) => [linkKey(link), link]));
  const afterLinks = new Map(after.connections.map((link) => [linkKey(link), link]));
  for (const [key, link] of beforeLinks) {
    if (afterLinks.get(key)?.type !== link.type) diff.push({ type: 'remove', text: `− ${describeLink(link, before, labels)}` });
  }
  for (const [key, link] of afterLinks) {
    if (beforeLinks.get(key)?.type !== link.type) diff.push({ type: 'add', text: `+ ${describeLink(link, after, labels)}` });
  }
  for (const [key, label] of [['annotations', '注释'], ['groups', '分组']]) {
    const removed = before[key].length - after[key].length;
    if (removed > 0) diff.push({ type: 'remove', text: `− ${removed} 个画布${label}` });
  }
  return diff;
}

export function bindGraphAssistant({ read, write, identity, nodeName, parameterName, portName, onVisibility }) {
  const open = mountAssistant(document.getElementById('btn-auto-assistant'), {
    mode: 'graph',
    defaultOpen: true,
    heading: '流程助手',
    title: '自动化编辑器',
    emptyTitle: '你想自动完成什么？',
    emptyDescription: '用平常说话的方式描述操作。AI 会搭建流程，你可以继续用对话调整。',
    examples: [
      ['按顺序执行操作', '先等待 1 秒，再输入「开始测试」，等 300 毫秒后按一次 Enter，然后结束。'],
      ['检测颜色并处理超时', '每 500 毫秒检测一次颜色，匹配后按 Enter，5 秒内没匹配就结束。请先问我颜色和区域。'],
      ['检查已有流程', '检查当前流程里有没有断开的步骤、缺少的条件或永远等不到的出口。把问题告诉我。'],
    ],
    onVisibility,
    applyLabel: '应用到画布',
    placeholder: '描述要执行的步骤，或告诉 AI 要改哪里…',
    scopes: [['new', '新流程'], ['current', '当前流程']],
    autoApply(before, scope) {
      const { nodes, connections, annotations, groups } = before.graph;
      return scope === 'current' || (!connections.length && !annotations.length && !groups.length &&
        nodes.every((node) => node.type === 'flow/start') &&
        !Object.keys(before.variables).length && !Object.keys(before.settings).length);
    },
    snapshot() {
      const doc = read();
      if (!doc) return { name: document.getElementById('script-name-input').value,
        revision: identity(), variables: {}, settings: {}, graph: structuredClone(emptyGraph) };
      return {
        name: doc.meta.name,
        revision: identity(),
        variables: doc.variables,
        settings: doc.settings,
        graph: {
          ...doc.graph,
          connections: doc.graph.connections.map(({ from, to, type }) => ({ from, to, type })),
        },
      };
    },
    attach(before) {
      const { variables, settings, graph } = before;
      return { data: graph.nodes.length ? { current: { variables, settings, graph } } : {} };
    },
    propose(reply, before) {
      const after = { ...reply.document, name: before.name };
      const labels = {
        node: (type) => reply.node_names[type] || nodeName(type),
        param: parameterName,
        port: portName,
      };
      return { after, diff: graphDiff(before.graph, after.graph, labels) };
    },
    stats(reply) {
      return `${reply.stats.nodes} 个节点 · ${reply.stats.connections} 条连线`;
    },
    apply(snapshot) {
      write(snapshot);
    },
  });
  document.getElementById('btn-manual-editor').addEventListener('click', () => open(false, false));
}
