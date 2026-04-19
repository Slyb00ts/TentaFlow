// =============================================================================
// Plik: lib/virtual-list.js
// Opis: Vanilla virtualizer pionowej listy. Cechy:
//       - Dynamic item heights via getItemHeight(index, item) callback
//         (cache + invalidation gdy resize)
//       - Overscan (default 5) — pre-render poza viewport dla plynnego scroll
//       - Total height = suma wszystkich itemSize (cumulative offsets)
//       - Scroll-to-bottom auto-pin (chat use case)
//       - ResizeObserver na container — invalidate cache + remeasure
//       - rAF-throttled scroll handler
//
//       Przyklad:
//         const list = createVirtualList(hostEl, {
//           items,
//           getItemHeight: (i, item) => measureItemHeight(item.text, {maxWidth}),
//           renderItem: (i, item) => `<div class="msg">${item.text}</div>`,
//           overscan: 5,
//           pinToBottom: true,
//         });
//         list.setItems(newItems);
//         list.append(newItem);
//         list.scrollToBottom();
//         list.destroy();
// =============================================================================

const DEFAULT_OVERSCAN = 5;

/// Tworzy wirtualizowana liste. Zwraca handle z metodami sterujacymi.
export function createVirtualList(host, opts) {
  if (!host) throw new Error('virtual-list: host is required');
  const overscan = opts.overscan ?? DEFAULT_OVERSCAN;
  let items = opts.items ?? [];
  const getItemHeight = opts.getItemHeight;
  const renderItem = opts.renderItem;
  const pinToBottom = opts.pinToBottom ?? false;
  const onScroll = opts.onScroll;

  if (typeof getItemHeight !== 'function') throw new Error('virtual-list: getItemHeight required');
  if (typeof renderItem !== 'function') throw new Error('virtual-list: renderItem required');

  // Setup DOM: scroll container + spacer (full height) + viewport (absolute positioned items)
  host.classList.add('vlist-host');
  host.innerHTML = `
    <div class="vlist-spacer" style="position:relative;width:100%;">
      <div class="vlist-viewport" style="position:absolute;top:0;left:0;right:0;"></div>
    </div>
  `;
  const spacer = host.querySelector('.vlist-spacer');
  const viewport = host.querySelector('.vlist-viewport');

  // Cache: index → height. Invalidated przy setItems/resize.
  let heightCache = new Float64Array(items.length);
  let offsetCache = new Float64Array(items.length + 1);
  let totalHeight = 0;
  let containerWidth = host.clientWidth || 0;

  // Pinning state
  let pinned = pinToBottom;
  let lastRenderRange = { start: -1, end: -1 };

  // rAF throttle
  let rafId = null;

  function recompute() {
    const len = items.length;
    if (heightCache.length !== len) heightCache = new Float64Array(len);
    if (offsetCache.length !== len + 1) offsetCache = new Float64Array(len + 1);
    let acc = 0;
    for (let i = 0; i < len; i++) {
      const h = getItemHeight(i, items[i]);
      heightCache[i] = h;
      offsetCache[i] = acc;
      acc += h;
    }
    offsetCache[len] = acc;
    totalHeight = acc;
    spacer.style.height = `${totalHeight}px`;
  }

  // Binary search: znajdz pierwszy index ktory offset >= scrollTop
  function findStartIndex(scrollTop) {
    let lo = 0;
    let hi = items.length - 1;
    if (hi < 0) return 0;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      const offset = offsetCache[mid];
      if (offset === scrollTop) return mid;
      if (offset < scrollTop) lo = mid + 1;
      else hi = mid - 1;
    }
    return Math.max(0, hi);
  }

  function render() {
    if (items.length === 0) {
      viewport.innerHTML = '';
      lastRenderRange = { start: 0, end: 0 };
      return;
    }
    const scrollTop = host.scrollTop;
    const viewportH = host.clientHeight;
    const startIdx = Math.max(0, findStartIndex(scrollTop) - overscan);
    let endIdx = startIdx;
    let acc = 0;
    while (endIdx < items.length && acc < viewportH + overscan * 2 * 50) {
      acc += heightCache[endIdx];
      endIdx += 1;
    }
    endIdx = Math.min(items.length, endIdx + overscan);

    // Skip if same range (avoid reflow)
    if (startIdx === lastRenderRange.start && endIdx === lastRenderRange.end) return;
    lastRenderRange = { start: startIdx, end: endIdx };

    const offsetTop = offsetCache[startIdx];
    viewport.style.transform = `translateY(${offsetTop}px)`;

    // Render — buduj string, jednorazowy innerHTML write
    const parts = [];
    for (let i = startIdx; i < endIdx; i++) {
      parts.push(`<div class="vlist-item" data-vidx="${i}" style="min-height:${heightCache[i]}px;">${renderItem(i, items[i])}</div>`);
    }
    viewport.innerHTML = parts.join('');
  }

  function onScrollHandler() {
    // Update pinned state — jesli user scrollnie w gore o > 50px, odpinamy
    const scrollTop = host.scrollTop;
    const distanceFromBottom = totalHeight - (scrollTop + host.clientHeight);
    pinned = distanceFromBottom < 30;
    if (rafId == null) {
      rafId = requestAnimationFrame(() => {
        rafId = null;
        render();
        onScroll?.(scrollTop, distanceFromBottom);
      });
    }
  }

  // ResizeObserver — gdy szerokosc kontenera sie zmienia, wszystkie wysokosci
  // moga sie zmienic (text wrapping). Recompute + render.
  const resizeObserver = new ResizeObserver((entries) => {
    const newWidth = entries[0].contentRect.width;
    if (Math.abs(newWidth - containerWidth) > 1) {
      containerWidth = newWidth;
      recompute();
      if (pinned) scrollToBottom();
      else render();
    }
  });
  resizeObserver.observe(host);

  host.addEventListener('scroll', onScrollHandler, { passive: true });

  // Initial render
  recompute();
  if (pinToBottom) scrollToBottom();
  else render();

  function setItems(next) {
    const wasPinned = pinned;
    items = next ?? [];
    recompute();
    if (wasPinned) scrollToBottom();
    else render();
  }

  function append(item) {
    items.push(item);
    const wasPinned = pinned;
    recompute();
    if (wasPinned) scrollToBottom();
    else render();
  }

  function appendBatch(newItems) {
    if (!newItems?.length) return;
    items.push(...newItems);
    const wasPinned = pinned;
    recompute();
    if (wasPinned) scrollToBottom();
    else render();
  }

  function scrollToBottom() {
    requestAnimationFrame(() => {
      host.scrollTop = host.scrollHeight;
      pinned = true;
      render();
    });
  }

  function scrollToIndex(idx) {
    if (idx < 0 || idx >= items.length) return;
    host.scrollTop = offsetCache[idx];
    render();
  }

  function destroy() {
    resizeObserver.disconnect();
    host.removeEventListener('scroll', onScrollHandler);
    if (rafId != null) cancelAnimationFrame(rafId);
    host.classList.remove('vlist-host');
    host.innerHTML = '';
  }

  return {
    setItems,
    append,
    appendBatch,
    scrollToBottom,
    scrollToIndex,
    destroy,
    refresh: () => {
      recompute();
      if (pinned) scrollToBottom();
      else render();
    },
    get items() { return items; },
    get pinned() { return pinned; },
  };
}
