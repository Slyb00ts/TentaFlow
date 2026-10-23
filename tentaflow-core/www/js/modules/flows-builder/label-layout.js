// ============ File: label-layout.js — places the outside labels of BPMN elements ============

// Whether the segment (x1,y1)-(x2,y2) passes through the rectangle (slab method).
export function segmentHitsRect(x1, y1, x2, y2, rect) {
  let near = 0; let far = 1;
  for (const [from, delta, low, high] of [[x1, x2 - x1, rect.left, rect.right], [y1, y2 - y1, rect.top, rect.bottom]]) {
    if (delta === 0) {
      if (from < low || from > high) return false;
    } else {
      const enter = (low - from) / delta; const leave = (high - from) / delta;
      near = Math.max(near, Math.min(enter, leave)); far = Math.min(far, Math.max(enter, leave));
      if (near > far) return false;
    }
  }
  return true;
}

const GAP = 12;
// A leader longer than this reads as pointing at something else; the label is
// deferred to hover and selection instead.
const MAX_LEADER = 260;
// A few pixels of a host's border are where a boundary event's leader starts.
const BORDER_GRACE = 3;

/**
 * Chooses a spot for each label, nearest to its element first. A spot is never
 * taken when the label would cover an element or another label, when the line
 * from the element to the label would run through another element or label, or
 * when the label would sit on the line of one placed before it: such a label gets
 * `null` and is shown on hover and selection only.
 *
 * `nodes`: every element as { id, left, top, right, bottom }.
 * `labels`: in placement order, { id, left, top, right, bottom, host, external,
 * width, height } where `host` is the rectangle of the task a boundary event
 * sits on, or null.
 * Returns a Map from element id to { rect, leader: { sourceX, sourceY, targetX, targetY } } or null.
 */
export function planProcessLabels({ nodes, labels }) {
  const occupied = [];
  const leaders = [];
  const placed = new Map();
  const overlaps = (left, right) => left.left < right.right + GAP && left.right + GAP > right.left
    && left.top < right.bottom + GAP && left.bottom + GAP > right.top;
  for (const item of labels) {
    const { width, height, host } = item;
    const centerX = (item.left + item.right) / 2;
    const centerY = (item.top + item.bottom) / 2;
    const nodeWidth = item.right - item.left;
    const nodeHeight = item.bottom - item.top;
    const side = host && Math.abs(centerX - host.right) <= 1 ? 'right'
      : host && Math.abs(centerX - host.left) <= 1 ? 'left'
        : host && Math.abs(centerY - host.top) <= 1 ? 'top'
          : host && Math.abs(centerY - host.bottom) <= 1 ? 'bottom'
            : null;
    const sides = side ? [side, ...['top', 'bottom', 'right', 'left'].filter((candidate) => candidate !== side)]
      : item.external ? ['top', 'bottom', 'left', 'right']
        : ['bottom', 'top', 'right', 'left'];
    // Beside the element on each side, slid along that side, and in further rows
    // behind the first. The side the element faces is preferred only by a few
    // pixels, so a crowded edge spreads its names in rows next to their events.
    const candidates = [];
    const steps = Math.min(nodes.length + labels.length + 4, 12);
    sides.forEach((direction, order) => {
      const horizontal = direction === 'top' || direction === 'bottom';
      for (let row = 0; row < 3; row += 1) {
        for (let slot = -steps; slot <= steps; slot += 1) {
          const along = slot * ((horizontal ? width : height) + GAP) / 2;
          const x = direction === 'left' ? item.left - width - GAP - row * (width + GAP)
            : direction === 'right' ? item.right + GAP + row * (width + GAP)
              : centerX - width / 2 + along;
          const y = direction === 'top' ? item.top - height - GAP - row * (height + GAP)
            : direction === 'bottom' ? item.bottom + GAP + row * (height + GAP)
              : centerY - height / 2 + along;
          const cost = Math.hypot(x + width / 2 - centerX, y + height / 2 - centerY) + order * 8;
          if (cost <= MAX_LEADER) candidates.push({ rect: { left: x, top: y, right: x + width, bottom: y + height }, cost });
        }
      }
    });
    candidates.sort((left, right) => left.cost - right.cost);
    // From the element's border to the label's nearest point.
    const leaderTo = (rect) => {
      const targetX = Math.max(rect.left, Math.min(centerX, rect.right));
      const targetY = Math.max(rect.top, Math.min(centerY, rect.bottom));
      return { targetX, targetY, sourceX: Math.max(item.left, Math.min(targetX, item.left + nodeWidth)),
        sourceY: Math.max(item.top, Math.min(targetY, item.top + nodeHeight)) };
    };
    const free = ({ rect }) => !nodes.some((other) => overlaps(rect, other)) && !occupied.some((other) => overlaps(rect, other));
    const crossesNode = ({ rect }) => {
      const { sourceX, sourceY, targetX, targetY } = leaderTo(rect);
      return nodes.some((other) => other.id !== item.id && segmentHitsRect(sourceX, sourceY, targetX, targetY, {
        left: other.left + BORDER_GRACE, top: other.top + BORDER_GRACE, right: other.right - BORDER_GRACE, bottom: other.bottom - BORDER_GRACE }));
    };
    const crossesLabel = ({ rect }) => {
      const { sourceX, sourceY, targetX, targetY } = leaderTo(rect);
      return occupied.some((other) => segmentHitsRect(sourceX, sourceY, targetX, targetY, other));
    };
    // A label that would sit on the line of an earlier label reads as that
    // label's pointer, so it is as unwelcome as a line through a label.
    const coversLeader = ({ rect }) => leaders.some((line) => segmentHitsRect(line.sourceX, line.sourceY, line.targetX, line.targetY, rect));
    const chosen = candidates.find((candidate) => free(candidate) && !crossesNode(candidate)
      && !crossesLabel(candidate) && !coversLeader(candidate));
    if (!chosen) { placed.set(item.id, null); continue; }
    occupied.push(chosen.rect);
    const leader = leaderTo(chosen.rect);
    leaders.push(leader);
    placed.set(item.id, { rect: chosen.rect, leader });
  }
  return placed;
}
