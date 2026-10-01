// =============================================================================
// File: modules/org-structure/layout.js
// Description: Tidy-tree layout for the organization chart. Top-down
//   Reingold–Tilford contour packing with variable node sizes: every subtree
//   keeps a per-row contour, siblings are packed left to right against the
//   merged contour of their predecessors and each parent is centred over its
//   first and last child. Two shapes are not plain children:
//   - staff nodes hang to the right of their manager's card, on the manager's
//     own row, so they widen the manager's block but never push its reports;
//   - a manager whose reports are all leaves gets them stacked under the card
//     (indented, spine on the left) — a team of eight stays one narrow column
//     instead of a row 1.7 kpx wide. Long stacks wrap into columns.
//   Contours are indexed by ROW, so a stack occupies the rows it visually
//   covers and neighbouring subtrees cannot slide underneath it.
//   The layout is pure: no DOM, no measuring. Callers give every node its size.
//   Sibling gaps are packed, not equalised (no Walker apportionment): a small
//   subtree between two large ones sits against its left neighbour.
// =============================================================================

export const DEFAULT_METRICS = Object.freeze({
  hgap: 24,
  vgap: 40,
  tightGap: 8,
  staffGap: 36,
  stackLeaves: true,
  stackIndent: 30,
  stackTopGap: 28,
  stackRowsMax: 8,
  stackColumnsMax: 4,
  pad: 24,
});

/**
 * A node is a leaf for stacking when nothing is laid out under it. A folded one (`more` > 0)
 * counts: its pill sits beside the card, so a whole team of managers still stacks compactly.
 */
function isLeaf(node) {
  return !(node.children?.length) && !(node.staff?.length);
}

function staffExtent(node, metrics) {
  let total = 0;
  for (const s of node.staff ?? []) total += metrics.staffGap + s.w;
  return total;
}

/** Column-major cells of a stack: the first column fills first, so short teams read top-down. */
function stackCells(children, metrics) {
  const n = children.length;
  const columns = Math.min(metrics.stackColumnsMax, Math.max(1, Math.ceil(n / metrics.stackRowsMax)));
  const rows = Math.ceil(n / columns);
  const columnWidths = new Array(columns).fill(0);
  children.forEach((child, i) => {
    const col = Math.floor(i / rows);
    columnWidths[col] = Math.max(columnWidths[col], child.w);
  });
  const columnLeft = [];
  let left = 0;
  for (const width of columnWidths) {
    columnLeft.push(left);
    left += width + metrics.hgap;
  }
  return { rows, columnLeft };
}

/**
 * Computes the shape of one node from the shapes of its children (already measured).
 * `lo[r]`/`hi[r]` are the extreme x of row `r` (0 = the node's own row) relative to the
 * node's anchor (centre of its card). Children get their offset from the anchor in `_rel`.
 */
function measure(node, metrics) {
  const half = node.w / 2;
  const lo = [-half];
  const hi = [half + staffExtent(node, metrics)];
  const kids = node.children ?? [];
  node._kind = 'plain';
  if (!kids.length) return { lo, hi };

  if (metrics.stackLeaves && kids.length >= 2 && kids.every(isLeaf)) {
    node._kind = 'stack';
    const { rows, columnLeft } = stackCells(kids, metrics);
    const base = -half + metrics.stackIndent;
    kids.forEach((child, i) => {
      const col = Math.floor(i / rows);
      const row = (i % rows) + 1;
      child._rel = base + columnLeft[col];
      child._row = row;
      const right = child._rel + child.w;
      lo[row] = lo[row] === undefined ? child._rel : Math.min(lo[row], child._rel);
      hi[row] = hi[row] === undefined ? right : Math.max(hi[row], right);
    });
    return { lo, hi };
  }

  const packed = packSiblings(kids.map((kid) => kid._shape), metrics.hgap);
  const anchors = packed.offsets;
  const centre = (anchors[0] + anchors[anchors.length - 1]) / 2;
  kids.forEach((kid, i) => { kid._rel = anchors[i] - centre; });
  for (let r = 0; r < packed.lo.length; r += 1) {
    lo[r + 1] = packed.lo[r] - centre;
    hi[r + 1] = packed.hi[r] - centre;
  }
  return { lo, hi };
}

/** Post-order over the forest without recursion, so a very deep chain cannot exhaust the call stack. */
function measureAll(roots, metrics) {
  const order = [];
  const pending = [...roots];
  while (pending.length) {
    const node = pending.pop();
    order.push(node);
    for (const kid of node.children ?? []) pending.push(kid);
  }
  for (let i = order.length - 1; i >= 0; i -= 1) order[i]._shape = measure(order[i], metrics);
  return roots.map((root) => root._shape);
}

/** Left-to-right placement of sibling shapes; returns each anchor and the merged contour. */
function packSiblings(shapes, gap) {
  const offsets = [];
  const lo = [];
  const hi = [];
  shapes.forEach((shape, i) => {
    let x = 0;
    if (i > 0) {
      x = -Infinity;
      const rows = Math.min(shape.lo.length, hi.length);
      for (let r = 0; r < rows; r += 1) x = Math.max(x, hi[r] + gap - shape.lo[r]);
    }
    offsets.push(x);
    for (let r = 0; r < shape.lo.length; r += 1) {
      const left = x + shape.lo[r];
      const right = x + shape.hi[r];
      lo[r] = lo[r] === undefined ? left : Math.min(lo[r], left);
      hi[r] = hi[r] === undefined ? right : Math.max(hi[r], right);
    }
  });
  return { offsets, lo, hi };
}

/**
 * Lays out a forest.
 * @param {Array<{id:string,w:number,h:number,children?:Array,staff?:Array,more?:number}>} roots
 * @param {Partial<typeof DEFAULT_METRICS>} [options]
 * @returns {{items:Array<{id:string,node:object,x:number,y:number,w:number,h:number,row:number,role:'node'|'staff'|'stacked',parentId:string|null}>,byId:Map<string,object>,width:number,height:number,rowTops:number[]}}
 */
export function layoutForest(roots, options = {}) {
  const metrics = { ...DEFAULT_METRICS, ...options };
  const shapes = measureAll(roots, metrics);
  const packed = packSiblings(shapes, metrics.hgap * 2);
  const items = [];
  const rowHeight = [];
  const rowGap = [];

  // A row's gap to the one above is the widest any of its cells asks for: a plain node needs
  // room for its "+N" pill, the first cell of a stack needs less, later stack cells the least.
  const note = (row, height, gap) => {
    rowHeight[row] = Math.max(rowHeight[row] ?? 0, height);
    rowGap[row] = Math.max(rowGap[row] ?? 0, gap);
  };

  // Iterative walk: a chain thousands of positions deep must not exhaust the call stack.
  const stack = roots.map((root, i) => ({ node: root, anchor: packed.offsets[i], row: 0, parentId: null, role: 'node' })).reverse();
  while (stack.length) {
    const { node, anchor, row, parentId, role } = stack.pop();
    const item = {
      id: node.id, node, x: anchor - node.w / 2, y: 0, w: node.w, h: node.h, row, role, parentId,
    };
    items.push(item);
    note(row, node.h, role !== 'stacked' ? metrics.vgap
      : (node._row === 1 ? metrics.stackTopGap : metrics.tightGap));

    if (node.staff?.length) {
      let left = anchor + node.w / 2;
      for (const s of node.staff) {
        left += metrics.staffGap;
        items.push({
          id: s.id, node: s, x: left, y: 0, w: s.w, h: s.h, row, role: 'staff', parentId: node.id,
        });
        note(row, s.h, metrics.vgap);
        left += s.w;
      }
    }

    const kids = node.children ?? [];
    if (node._kind === 'stack') {
      for (const kid of kids) {
        // Stack cells are anchored by their left edge; convert to the anchor the item code expects.
        stack.push({ node: kid, anchor: anchor + kid._rel + kid.w / 2, row: row + kid._row, parentId: node.id, role: 'stacked' });
      }
    } else {
      for (let i = kids.length - 1; i >= 0; i -= 1) {
        stack.push({ node: kids[i], anchor: anchor + kids[i]._rel, row: row + 1, parentId: node.id, role: 'node' });
      }
    }
  }

  const rowTops = [];
  let y = metrics.pad;
  for (let r = 0; r < rowHeight.length; r += 1) {
    if (r > 0) y += rowGap[r] ?? metrics.vgap;
    rowTops[r] = y;
    y += rowHeight[r] ?? 0;
  }

  let minX = Infinity;
  let maxX = -Infinity;
  let maxY = 0;
  for (const item of items) {
    item.y = rowTops[item.row];
    minX = Math.min(minX, item.x);
    maxX = Math.max(maxX, item.x + item.w);
    maxY = Math.max(maxY, item.y + item.h);
  }
  if (!items.length) return { items, byId: new Map(), width: metrics.pad * 2, height: metrics.pad * 2, rowTops };

  const shift = metrics.pad - minX;
  for (const item of items) item.x += shift;
  const byId = new Map(items.map((item) => [item.id, item]));
  return { items, byId, width: maxX - minX + metrics.pad * 2, height: maxY + metrics.pad, rowTops };
}

/** Swaps width and height of every node of a forest (children and staff included), in place. */
export function swapSizes(roots) {
  const pending = [...roots];
  while (pending.length) {
    const node = pending.pop();
    [node.w, node.h] = [node.h, node.w];
    pending.push(...(node.children ?? []), ...(node.staff ?? []));
  }
}

/** Turns a finished layout a quarter over its diagonal: x and y, width and height trade places. */
export function transposeLayout(layout) {
  for (const item of layout.items) {
    [item.x, item.y] = [item.y, item.x];
    [item.w, item.h] = [item.h, item.w];
  }
  [layout.width, layout.height] = [layout.height, layout.width];
}
