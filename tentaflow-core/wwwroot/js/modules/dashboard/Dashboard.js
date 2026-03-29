// =============================================================================
// Plik: modules/dashboard/Dashboard.js
// Opis: Widok glowny dashboardu - kafelki metryk, wykres tokens/s,
//       tabela serwisów ze statusem, gauge GPU.
// Przyklad: ViewRouter.register('dashboard', Dashboard);
// =============================================================================

const Dashboard = (() => {
  'use strict';

  let metricsHandler = null;
  let resizeHandler = null;
  let chartCtx = null;
  let chartData = [];
  const MAX_CHART_POINTS = 60;

  // Stale paddingu wykresu
  const CHART_PADDING = { top: 10, right: 10, bottom: 20, left: 50 };

  // Cache gradientu - odswiezany w resizeChart
  let cachedGradient = null;

  // Flaga requestAnimationFrame do throttlowania drawChart
  let drawChartScheduled = false;

  // Timer debounce resize
  let resizeDebounceTimer = null;

  // Renderowanie HTML widoku
  function render() {
    return `
      <div class="metrics-grid">
        <div class="metric-card">
          <div class="metric-label" data-i18n="dashboard.metrics.tokens_in">${I18n.t('dashboard.metrics.tokens_in')}</div>
          <div class="metric-value accent" id="metric-tokens-in">0</div>
          <div class="metric-sub" data-i18n="dashboard.metrics.tokens_in_sub">${I18n.t('dashboard.metrics.tokens_in_sub')}</div>
        </div>
        <div class="metric-card">
          <div class="metric-label" data-i18n="dashboard.metrics.tokens_out">${I18n.t('dashboard.metrics.tokens_out')}</div>
          <div class="metric-value accent" id="metric-tokens-out">0</div>
          <div class="metric-sub" data-i18n="dashboard.metrics.tokens_out_sub">${I18n.t('dashboard.metrics.tokens_out_sub')}</div>
        </div>
        <div class="metric-card">
          <div class="metric-label" data-i18n="dashboard.metrics.active_requests">${I18n.t('dashboard.metrics.active_requests')}</div>
          <div class="metric-value" id="metric-active-requests">0</div>
          <div class="metric-sub" data-i18n="dashboard.metrics.active_requests_sub">${I18n.t('dashboard.metrics.active_requests_sub')}</div>
        </div>
        <div class="metric-card">
          <div class="metric-label" data-i18n="dashboard.metrics.avg_latency">${I18n.t('dashboard.metrics.avg_latency')}</div>
          <div class="metric-value" id="metric-latency">0 ms</div>
          <div class="metric-sub" data-i18n="dashboard.metrics.avg_latency_sub">${I18n.t('dashboard.metrics.avg_latency_sub')}</div>
        </div>
        <div class="metric-card">
          <div class="metric-label" data-i18n="dashboard.metrics.active_services">${I18n.t('dashboard.metrics.active_services')}</div>
          <div class="metric-value success" id="metric-active-services">0</div>
          <div class="metric-sub" data-i18n="dashboard.metrics.active_services_sub">${I18n.t('dashboard.metrics.active_services_sub')}</div>
        </div>
        <div class="metric-card">
          <div class="metric-label" data-i18n="dashboard.metrics.total_requests">${I18n.t('dashboard.metrics.total_requests')}</div>
          <div class="metric-value" id="metric-total-requests">0</div>
          <div class="metric-sub" data-i18n="dashboard.metrics.total_requests_sub">${I18n.t('dashboard.metrics.total_requests_sub')}</div>
        </div>
        <div class="metric-card">
          <div class="metric-label" data-i18n="dashboard.metrics.tokens_total">${I18n.t('dashboard.metrics.tokens_total')}</div>
          <div class="metric-value" id="metric-total-tokens">0 / 0</div>
          <div class="metric-sub" data-i18n="dashboard.metrics.tokens_total_sub">${I18n.t('dashboard.metrics.tokens_total_sub')}</div>
        </div>
        <div class="metric-card">
          <div class="metric-label" data-i18n="dashboard.metrics.errors">${I18n.t('dashboard.metrics.errors')}</div>
          <div class="metric-value error" id="metric-errors">0</div>
          <div class="metric-sub" data-i18n="dashboard.metrics.errors_sub">${I18n.t('dashboard.metrics.errors_sub')}</div>
        </div>
      </div>

      <div class="chart-container">
        <div class="chart-header">
          <h3 data-i18n="dashboard.chart.title">${I18n.t('dashboard.chart.title')}</h3>
        </div>
        <div class="chart-canvas-wrapper">
          <canvas id="tokens-chart"></canvas>
        </div>
      </div>

      <div class="services-overview">
        <div class="services-overview-header">
          <h3 data-i18n="dashboard.services_list.title">${I18n.t('dashboard.services_list.title')}</h3>
          <span class="badge badge-info" id="services-count">0</span>
        </div>
        <div class="table-wrapper">
          <table>
            <thead>
              <tr>
                <th data-i18n="common.name">${I18n.t('common.name')}</th>
                <th data-i18n="common.type">${I18n.t('common.type')}</th>
                <th data-i18n="common.status">${I18n.t('common.status')}</th>
                <th data-i18n="common.strategy">${I18n.t('common.strategy')}</th>
                <th data-i18n="common.backends">${I18n.t('common.backends')}</th>
                <th data-i18n="common.latency">${I18n.t('common.latency')}</th>
              </tr>
            </thead>
            <tbody id="services-tbody">
              <tr>
                <td colspan="6">
                  <div class="empty-state">
                    <div class="empty-state-text" data-i18n="dashboard.services_list.loading">${I18n.t('dashboard.services_list.loading')}</div>
                  </div>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>
    `;
  }

  // Montowanie - uruchom pobieranie danych i WS
  function mount() {
    loadServices();
    initChart();

    metricsHandler = (data) => updateMetrics(data);
    WsClient.on('metrics', metricsHandler);
  }

  // Odmontowanie - zatrzymaj nasluchiWanie
  function unmount() {
    if (metricsHandler) {
      WsClient.off('metrics', metricsHandler);
      metricsHandler = null;
    }
    if (resizeHandler) {
      window.removeEventListener('resize', resizeHandler);
      resizeHandler = null;
    }
    chartCtx = null;
    chartData = [];
    cachedGradient = null;
    drawChartScheduled = false;
    clearTimeout(resizeDebounceTimer);
    resizeDebounceTimer = null;
  }

  // Zaladowanie listy serwisów z API
  async function loadServices() {
    try {
      const services = await ApiClient.get('/api/services');
      renderServicesTable(services);
    } catch (err) {
      console.error(I18n.t('dashboard.services_list.loading'), err);
      renderServicesTable([]);
    }
  }

  // Renderowanie tabeli serwisów
  function renderEmptyState() {
    return `
      <tr>
        <td colspan="6">
          <div class="empty-state">
            <div class="empty-state-text" data-i18n="dashboard.services_list.empty">${I18n.t('dashboard.services_list.empty')}</div>
            <div class="empty-state-hint" data-i18n="dashboard.services_list.empty_hint">${I18n.t('dashboard.services_list.empty_hint')}</div>
          </div>
        </td>
      </tr>
    `;
  }

  function renderServicesTable(services) {
    const tbody = document.getElementById('services-tbody');
    const countEl = document.getElementById('services-count');

    if (!tbody) return;

    if (!services || services.length === 0) {
      tbody.innerHTML = renderEmptyState();
      if (countEl) countEl.textContent = `0 ${I18n.t('dashboard.services_list.title').toLowerCase()}`;
      return;
    }

    if (countEl) {
      countEl.textContent = I18n.t('dashboard.services_list.count').replace('{count}', services.length);
    }

    tbody.innerHTML = services.map(s => {
      const statusClass = s.status === 'active' ? 'green' : s.status === 'maintenance' ? 'yellow' : 'red';
      const statusLabel = s.status === 'active' ? I18n.t('common.active') : s.status === 'maintenance' ? I18n.t('common.maintenance') : I18n.t('common.inactive');
      return `
        <tr>
          <td><strong>${Utils.escapeHtml(s.name)}</strong></td>
          <td>${Utils.escapeHtml(s.service_type)}</td>
          <td>
            <span class="badge badge-${s.status === 'active' ? 'success' : s.status === 'maintenance' ? 'warning' : 'error'}">
              <span class="status-dot status-dot-${statusClass}"></span>
              ${statusLabel}
            </span>
          </td>
          <td>${Utils.escapeHtml(s.strategy)}</td>
          <td>${s.backend_count || '-'}</td>
          <td>${s.avg_latency ? s.avg_latency + ' ms' : '-'}</td>
        </tr>
      `;
    }).join('');
  }

  // Aktualizacja metryk z WebSocket
  function updateMetrics(data) {
    const locale = I18n.getLanguage() === 'pl' ? 'pl-PL' : 'en-US';
    setMetric('metric-tokens-in', data.tokens_in_per_sec || 0, locale);
    setMetric('metric-tokens-out', data.tokens_out_per_sec || 0, locale);
    setMetric('metric-active-services', data.active_services || 0, locale);
    setMetric('metric-active-requests', data.active_requests || 0, locale);
    setMetric('metric-total-requests', data.total_requests || 0, locale);
    setMetric('metric-errors', data.total_errors || 0, locale);

    const latency = data.avg_latency_ms || 0;
    const latencyEl = document.getElementById('metric-latency');
    if (latencyEl) latencyEl.textContent = `${latency} ms`;

    const totalTokensEl = document.getElementById('metric-total-tokens');
    if (totalTokensEl) {
      const tin = (data.total_input_tokens || 0).toLocaleString(locale);
      const tout = (data.total_output_tokens || 0).toLocaleString(locale);
      totalTokensEl.textContent = `${tin} / ${tout}`;
    }

    const total = (data.tokens_in_per_sec || 0) + (data.tokens_out_per_sec || 0);
    addChartPoint(total);
    scheduleDrawChart();
  }

  // Ustawienie wartosci metryki
  function setMetric(id, value, locale) {
    const el = document.getElementById(id);
    if (el) el.textContent = typeof value === 'number' ? value.toLocaleString(locale) : value;
  }

  // Inicjalizacja canvas wykresu
  function initChart() {
    const canvas = document.getElementById('tokens-chart');
    if (!canvas) return;

    chartCtx = canvas.getContext('2d');
    chartData = new Array(MAX_CHART_POINTS).fill(0);

    // Rozmiar canvas (resizeChart wywoluje drawChart)
    resizeChart(canvas);
    resizeHandler = () => {
      clearTimeout(resizeDebounceTimer);
      resizeDebounceTimer = setTimeout(() => resizeChart(canvas), 150);
    };
    window.addEventListener('resize', resizeHandler);
  }

  // Dopasowanie rozmiaru canvas do kontenera
  function resizeChart(canvas) {
    const wrapper = canvas.parentElement;
    if (!wrapper) return;
    canvas.width = wrapper.clientWidth;
    canvas.height = wrapper.clientHeight;

    // Odswiez cache gradientu po zmianie rozmiaru
    cachedGradient = chartCtx.createLinearGradient(0, CHART_PADDING.top, 0, canvas.height - CHART_PADDING.bottom);
    cachedGradient.addColorStop(0, 'rgba(99, 102, 241, 0.3)');
    cachedGradient.addColorStop(1, 'rgba(99, 102, 241, 0.0)');

    drawChart();
  }

  // Zaplanuj rysowanie wykresu przez requestAnimationFrame
  function scheduleDrawChart() {
    if (drawChartScheduled) return;
    drawChartScheduled = true;
    requestAnimationFrame(() => {
      drawChartScheduled = false;
      drawChart();
    });
  }

  // Dodanie punktu do danych wykresu
  function addChartPoint(value) {
    chartData.push(value);
    if (chartData.length > MAX_CHART_POINTS) {
      chartData.shift();
    }
  }

  // Rysowanie wykresu liniowego na canvas
  function drawChart() {
    if (!chartCtx) return;

    const canvas = chartCtx.canvas;
    const w = canvas.width;
    const h = canvas.height;
    const chartW = w - CHART_PADDING.left - CHART_PADDING.right;
    const chartH = h - CHART_PADDING.top - CHART_PADDING.bottom;

    // Czyszczenie
    chartCtx.clearRect(0, 0, w, h);

    // Oblicz max wartosc
    const maxVal = Math.max(10, ...chartData) * 1.1;

    // Siatka pozioma
    chartCtx.strokeStyle = 'rgba(42, 46, 63, 0.5)';
    chartCtx.lineWidth = 1;
    const gridLines = 4;
    for (let i = 0; i <= gridLines; i++) {
      const y = CHART_PADDING.top + (chartH / gridLines) * i;
      chartCtx.beginPath();
      chartCtx.moveTo(CHART_PADDING.left, y);
      chartCtx.lineTo(w - CHART_PADDING.right, y);
      chartCtx.stroke();

      // Etykieta
      const val = Math.round(maxVal - (maxVal / gridLines) * i);
      chartCtx.fillStyle = '#5c6078';
      chartCtx.font = '11px Manrope, sans-serif';
      chartCtx.textAlign = 'right';
      chartCtx.fillText(val.toString(), CHART_PADDING.left - 8, y + 4);
    }

    // Linia wykresu
    if (chartData.length < 2) return;

    const stepX = chartW / (MAX_CHART_POINTS - 1);

    // Oblicz punkty raz - uzyte w wypelnieniu i linii
    const points = new Array(chartData.length);
    for (let i = 0; i < chartData.length; i++) {
      points[i] = {
        x: CHART_PADDING.left + i * stepX,
        y: CHART_PADDING.top + chartH - (chartData[i] / maxVal) * chartH
      };
    }

    // Sciezka wypelnienia
    chartCtx.beginPath();
    chartCtx.moveTo(CHART_PADDING.left, h - CHART_PADDING.bottom);

    for (let i = 0; i < points.length; i++) {
      chartCtx.lineTo(points[i].x, points[i].y);
    }

    chartCtx.lineTo(points[points.length - 1].x, h - CHART_PADDING.bottom);
    chartCtx.closePath();
    chartCtx.fillStyle = cachedGradient;
    chartCtx.fill();

    // Linia
    chartCtx.beginPath();
    chartCtx.moveTo(points[0].x, points[0].y);
    for (let i = 1; i < points.length; i++) {
      chartCtx.lineTo(points[i].x, points[i].y);
    }
    chartCtx.strokeStyle = '#6366f1';
    chartCtx.lineWidth = 2;
    chartCtx.stroke();
  }

  return { render, mount, unmount };
})();
