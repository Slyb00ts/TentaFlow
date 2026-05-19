// =============================================================================
// Plik: components/tf-heatmap.js
// Opis: Heatmapa 2D (Specialized::Heatmap, WeeklyScheduleGrid). Property `values`
// jako Array<Array<number>> [rows][cols]; klik komorki emituje CustomEvent
// 'cell-click' z { row, col, value }.
// =============================================================================

class TfHeatmap extends HTMLElement {
  constructor() {
    super();
    this._values = [];
    this._rowLabels = [];
    this._colLabels = [];
    this._scale = 'sequential';
    this._showLegend = false;
    this._onCellClick = null;
    this._grid = null;
  }

  connectedCallback() {
    if (!this._grid) {
      this._grid = document.createElement('div');
      this._grid.className = 'sdk-heatmap-grid';
      this._grid.style.display = 'grid';
      this._grid.style.gap = '2px';
      this.appendChild(this._grid);
    }
    this._render();
  }

  set values(value) { this._values = Array.isArray(value) ? value : []; if (this.isConnected) this._render(); }
  set rowLabels(value) { this._rowLabels = Array.isArray(value) ? value : []; if (this.isConnected) this._render(); }
  set colLabels(value) { this._colLabels = Array.isArray(value) ? value : []; if (this.isConnected) this._render(); }
  set colorScale(value) { this._scale = String(value || 'sequential'); if (this.isConnected) this._render(); }
  set onCellClick(callback) { this._onCellClick = typeof callback === 'function' ? callback : null; }

  _cellColor(v, min, max) {
    const t = max === min ? 0 : (v - min) / (max - min);
    switch (this._scale) {
      case 'diverging':
        if (t < 0.5) return `rgba(96,165,250,${0.2 + (0.5 - t) * 1.6})`;
        return `rgba(239,68,68,${0.2 + (t - 0.5) * 1.6})`;
      case 'heat': {
        const r = Math.round(40 + 215 * t);
        const g = Math.round(80 - 60 * t);
        const b = Math.round(140 - 100 * t);
        return `rgb(${r},${g},${b})`;
      }
      case 'categorical': {
        const palette = ['#6366f1', '#22c55e', '#f59e0b', '#ef4444', '#a78bfa', '#60a5fa'];
        return palette[Math.round(t * (palette.length - 1)) % palette.length];
      }
      case 'sequential':
      default:
        return `rgba(99,102,241,${0.1 + t * 0.85})`;
    }
  }

  _render() {
    const g = this._grid;
    if (!g) return;
    g.innerHTML = '';
    const rows = this._values.length;
    if (rows === 0) return;
    const cols = (this._values[0] || []).length;
    g.style.gridTemplateColumns = `repeat(${cols}, minmax(12px, 1fr))`;
    let min = Infinity, max = -Infinity;
    for (const row of this._values) for (const v of row) {
      if (Number.isFinite(v)) { if (v < min) min = v; if (v > max) max = v; }
    }
    if (!Number.isFinite(min)) { min = 0; max = 1; }
    for (let r = 0; r < rows; r++) {
      for (let c = 0; c < cols; c++) {
        const v = Number(this._values[r][c]) || 0;
        const cell = document.createElement('div');
        cell.className = 'sdk-heatmap-cell';
        cell.style.aspectRatio = '1';
        cell.style.background = this._cellColor(v, min, max);
        cell.style.borderRadius = '2px';
        cell.style.cursor = this._onCellClick ? 'pointer' : 'default';
        cell.title = `${this._rowLabels[r] ?? r}/${this._colLabels[c] ?? c}: ${v}`;
        if (this._onCellClick) {
          cell.addEventListener('click', () => this._onCellClick({ row: r, col: c, value: v }));
        }
        g.appendChild(cell);
      }
    }
  }
}

if (!customElements.get('tf-heatmap')) {
  customElements.define('tf-heatmap', TfHeatmap);
}

export { TfHeatmap };
