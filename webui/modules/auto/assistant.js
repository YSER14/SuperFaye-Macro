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

export function bindGraphAssistant({ read, write, nodeName, parameterName, portName, onVisibility }) {
  mountAssistant(document.getElementById('btn-auto-assistant'), {
    mode: 'graph',
    title: '自动化编辑器',
    emptyTitle: '把操作写成流程',
    examples: [
      ['新建带超时的流程', '起草颜色检测流程，检测间隔 500 ms，未匹配满 5 秒就结束。先向我确认颜色和区域。', 'new'],
      ['给当前流程加超时', '检查现有流程，给会长时间等待的步骤加 5 秒超时出口。', 'current'],
    ],
    onVisibility,
    applyLabel: '应用到画布',
    placeholder: '例如，每隔半秒检测指定区域的颜色，三秒没匹配就结束。',
    scopes: [['new', '无 · 起草新流程'], ['current', '当前画布']],
    snapshot() {
      const doc = read();
      if (!doc) return { name: document.getElementById('script-name-input').value,
        variables: {}, settings: {}, graph: structuredClone(emptyGraph) };
      return {
        name: doc.meta.name,
        variables: doc.variables,
        settings: doc.settings,
        graph: {
          ...doc.graph,
          connections: doc.graph.connections.map(({ from, to, type }) => ({ from, to, type })),
        },
      };
    },
    attach(scope, before) {
      if (scope !== 'current') return { data: null };
      if (!before.graph.nodes.length) throw new Error('当前画布没有可以附带的节点。');
      return { data: { variables: before.variables, settings: before.settings, graph: before.graph } };
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
}
