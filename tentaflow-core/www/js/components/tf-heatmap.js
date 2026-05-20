// =============================================================================
// Plik: components/tf-heatmap.js
// Opis: Heatmapa 2D (Specialized::Heatmap, WeeklyScheduleGrid). Renderuje siatke
// row_label + col_header + komorki z 5-stopniowa skala kolorow zgodna z mockup
// (m01 activity heatmap). Klik komorki emituje CustomEvent 'cell-click' przez
// callback `onCellClick`. Legendę (niska→wysoka) wlacza `showLegend`.
// =============================================================================

class TfHeatmap extends HTMLElement {
  constructor() {
    super();
    this._values = [];
    this._rowLabels = [];
    this._colLabels = [];
    this._rows = null;
    this._cols = null;
    this._scale = 'sequential';
    this._showLegend = false;
    this._onCellClick = null;
  }

  connectedCallback() {
    if (!this._rendered) this._render();
  }

  set values(value) { this._values = Array.isArray(value) ? value : []; this._render(); }
  set rowLabels(value) { this._rowLabels = Array.isArray(value) ? value : []; this._render(); }
  set colLabels(value) { this._colLabels = Array.isArray(value) ? value : []; this._render(); }
  set rows(value) { this._rows = Number(value) || null; this._render(); }
  set cols(value) { this._cols = Number(value) || null; this._render(); }
  set colorScale(value) { this._scale = String(value || 'sequential'); this._render(); }
  set showLegend(value) { this._showLegend = Boolean(value); this._render(); }
  set onCellClick(callback) { this._onCellClick = typeof callback === 'function' ? callback : null; this._render(); }

  _levelFor(v) {
    // Match the m01 mockup buckets — low values render as transparent so the
    // backdrop gap colour shows through (blank-looking cells).
    if (v > 0.75) return 4;
    if (v > 0.5) return 3;
    if (v > 0.3) return 2;
    if (v > 0.15) return 1;
    return 0;
  }

  _render() {
    this._rendered = true;
    const values = this._values;
    const rows = this._rows ?? values.length;
    const cols = this._cols ?? (values[0] || []).length;
    if (!rows || !cols) {
      this.innerHTML = '';
      return;
    }

    const hasRowLabels = this._rowLabels.length > 0;
    const hasColLabels = this._colLabels.length > 0;

    this.innerHTML = '';

    // Outer wrap allows legend to float above-right of the grid without
    // disturbing column alignment.
    const wrap = document.createElement('div');
    wrap.className = 'tf-heatmap-wrap';
    wrap.style.position = 'relative';

    if (this._showLegend) {
      wrap.appendChild(this._renderLegend());
    }

    const grid = document.createElement('div');
    grid.className = 'tf-heatmap-grid';
    grid.style.display = 'grid';
    const colTemplate = hasRowLabels
      ? `minmax(80px, max-content) repeat(${cols}, minmax(12px, 1fr))`
      : `repeat(${cols}, minmax(12px, 1fr))`;
    grid.style.gridTemplateColumns = colTemplate;
    grid.style.gap = '2px';

    if (hasColLabels) {
      if (hasRowLabels) grid.appendChild(this._labelCell('', 'corner'));
      for (let c = 0; c < cols; c++) {
        grid.appendChild(this._labelCell(this._colLabels[c] || '', 'col'));
      }
    }

    for (let r = 0; r < rows; r++) {
      if (hasRowLabels) {
        grid.appendChild(this._labelCell(this._rowLabels[r] || '', 'row'));
      }
      const rowValues = values[r] || [];
      for (let c = 0; c < cols; c++) {
        const v = Number(rowValues[c]) || 0;
        grid.appendChild(this._cell(r, c, v, cols));
      }
    }

    wrap.appendChild(grid);
    this.appendChild(wrap);
  }

  _labelCell(text, kind) {
    const el = document.createElement('div');
    el.className = `tf-heatmap-label tf-heatmap-label-${kind}`;
    el.textContent = text;
    return el;
  }

  _cell(r, c, v, cols) {
    const cell = document.createElement('div');
    cell.className = 'tf-heatmap-cell';
    const level = this._levelFor(v);
    if (level > 0) cell.dataset.level = String(level);
    cell.style.cursor = this._onCellClick ? 'pointer' : 'default';
    cell.title = `${this._rowLabels[r] ?? r} · ${this._colLabels[c] ?? c}: ${v.toFixed(2)}`;
    if (this._onCellClick) {
      cell.addEventListener('click', () => this._onCellClick({ row: r, col: c, value: v }));
    }
    cell.style.animationDelay = `${(r * cols + c) * 4}ms`;
    return cell;
  }

  _renderLegend() {
    const legend = document.createElement('div');
    legend.className = 'tf-heatmap-legend';
    const lo = document.createElement('span');
    lo.className = 'tf-heatmap-legend-label';
    lo.textContent = 'niska';
    legend.appendChild(lo);
    for (let i = 1; i <= 4; i++) {
      const sw = document.createElement('span');
      sw.className = 'tf-heatmap-legend-swatch';
      sw.dataset.level = String(i);
      legend.appendChild(sw);
    }
    const hi = document.createElement('span');
    hi.className = 'tf-heatmap-legend-label';
    hi.textContent = 'wysoka';
    legend.appendChild(hi);
    return legend;
  }
}

if (!customElements.get('tf-heatmap')) {
  customElements.define('tf-heatmap', TfHeatmap);
}

export { TfHeatmap };
