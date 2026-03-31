// =============================================================================
// Plik: SparklineChart.js
// Opis: Komponent sparkline — mini-wykres na canvas z ring buforem 30 probek,
//       obsluga HiDPI, progow kolorow i trybu dual-line (siec RX/TX).
// Przyklad: const s = new SparklineChart(canvas, { color: '#22c55e', maxValue: 100 });
// =============================================================================

class SparklineChart {
  constructor(canvas, options = {}) {
    this.canvas = canvas;
    this.ctx = canvas.getContext('2d');
    this.options = {
      color: options.color || '#22c55e',
      maxValue: options.maxValue || 100,
      thresholds: options.thresholds || null,
      dualLine: options.dualLine || false,
      bufferSize: 30,
    };

    // Ring bufor — 30 probek = 60s historii (odswiezanie co 2s)
    this.buffer = [];
    this.bufferB = [];
    this.sampleCount = 0;

    // Wymiary logiczne
    this.logicalWidth = 120;
    this.logicalHeight = 30;

    this._setupCanvas();
    this.render();
  }

  // Konfiguracja canvas z uwzglednieniem devicePixelRatio
  _setupCanvas() {
    const dpr = window.devicePixelRatio || 1;
    this.canvas.width = this.logicalWidth * dpr;
    this.canvas.height = this.logicalHeight * dpr;
    this.canvas.style.width = this.logicalWidth + 'px';
    this.canvas.style.height = this.logicalHeight + 'px';
    this.ctx.scale(dpr, dpr);
  }

  // Kolor na podstawie progow
  _getColor(value) {
    if (!this.options.thresholds) return this.options.color;
    const t = this.options.thresholds;
    if (value > t.high) return getComputedStyle(document.documentElement).getPropertyValue('--color-error').trim() || '#ef4444';
    if (value > t.medium) return getComputedStyle(document.documentElement).getPropertyValue('--color-warning').trim() || '#f59e0b';
    return getComputedStyle(document.documentElement).getPropertyValue('--color-success').trim() || '#22c55e';
  }

  // Dodanie jednej wartosci do bufora
  push(value) {
    this.sampleCount++;
    if (this.buffer.length >= this.options.bufferSize) {
      this.buffer.shift();
    }
    this.buffer.push(value);
    this._updateAriaLabel(value);
    this.render();
  }

  // Dodanie dwoch wartosci (RX + TX) do buforow
  pushDual(valueA, valueB) {
    this.sampleCount++;
    if (this.buffer.length >= this.options.bufferSize) {
      this.buffer.shift();
      this.bufferB.shift();
    }
    this.buffer.push(valueA);
    this.bufferB.push(valueB);
    this._updateAriaLabel(valueA);
    this.render();
  }

  // Aktualizacja aria-label z aktualna wartoscia i trendem
  _updateAriaLabel(currentValue) {
    let trend = 'stable';
    if (this.buffer.length >= 3) {
      const prev = this.buffer[this.buffer.length - 3];
      const diff = currentValue - prev;
      if (diff > 2) trend = 'rising';
      else if (diff < -2) trend = 'falling';
    }
    const label = `${Math.round(currentValue)}, trend: ${trend}`;
    this.canvas.setAttribute('aria-label', label);
  }

  // Rysowanie linii i wypelnienia pod nia
  _drawLine(data, maxY, color, dashed, lineWidth) {
    const ctx = this.ctx;
    const w = this.logicalWidth;
    const h = this.logicalHeight;
    const len = data.length;
    if (len < 2) return;

    // Rysuj od prawej strony, puste miejsce z lewej
    const totalSlots = this.options.bufferSize;
    const stepX = w / (totalSlots - 1);
    const startX = w - (len - 1) * stepX;

    ctx.beginPath();
    if (dashed) ctx.setLineDash([4, 3]);
    else ctx.setLineDash([]);

    ctx.strokeStyle = color;
    ctx.lineWidth = lineWidth || 2;
    ctx.lineJoin = 'round';
    ctx.lineCap = 'round';

    for (let i = 0; i < len; i++) {
      const x = startX + i * stepX;
      const y = h - (data[i] / maxY) * (h - 4) - 2;
      if (i === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    }
    ctx.stroke();

    // Wypelnienie pod linia (tylko dla solid)
    if (!dashed) {
      ctx.lineTo(startX + (len - 1) * stepX, h);
      ctx.lineTo(startX, h);
      ctx.closePath();
      ctx.fillStyle = color.replace(')', ', 0.1)').replace('rgb(', 'rgba(');
      // Jesli kolor jest hex, konwertuj
      if (color.startsWith('#')) {
        const r = parseInt(color.slice(1, 3), 16);
        const g = parseInt(color.slice(3, 5), 16);
        const b = parseInt(color.slice(5, 7), 16);
        ctx.fillStyle = `rgba(${r}, ${g}, ${b}, 0.1)`;
      }
      ctx.fill();
    }
    ctx.setLineDash([]);
  }

  // Pelne przerysowanie
  render() {
    const ctx = this.ctx;
    const w = this.logicalWidth;
    const h = this.logicalHeight;

    ctx.clearRect(0, 0, w, h);

    // Stan poczatkowy — brak probek
    if (this.buffer.length === 0) {
      ctx.fillStyle = 'rgba(92, 96, 120, 0.3)';
      ctx.font = '10px sans-serif';
      ctx.textAlign = 'center';
      ctx.textBaseline = 'middle';
      ctx.fillText(I18n.t('mesh.collecting'), w / 2, h / 2);
      return;
    }

    if (this.options.dualLine) {
      // Tryb sieci — dwie linie (RX + TX)
      const allVals = [...this.buffer, ...this.bufferB];
      const maxY = Math.max(...allVals, 1);

      const rxColor = getComputedStyle(document.documentElement).getPropertyValue('--color-info').trim() || '#3b82f6';
      const txColor = '#22d3ee';

      this._drawLine(this.buffer, maxY, rxColor, false, 2);
      this._drawLine(this.bufferB, maxY, txColor, true, 1);
    } else {
      // Tryb pojedynczej linii z progami
      const maxY = this.options.maxValue || Math.max(...this.buffer, 1);
      const lastValue = this.buffer[this.buffer.length - 1];
      const color = this._getColor(lastValue);
      this._drawLine(this.buffer, maxY, color, false, 2);
    }
  }

  // Czyszczenie zasobow
  destroy() {
    this.buffer = [];
    this.bufferB = [];
    this.sampleCount = 0;
    if (this.ctx) {
      this.ctx.clearRect(0, 0, this.canvas.width, this.canvas.height);
    }
  }
}
