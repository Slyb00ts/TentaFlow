// =============================================================================
// Plik: utils.js
// Opis: Wspolne funkcje uzywane przez wszystkie moduly dashboardu.
// =============================================================================

const Utils = (() => {
  function escapeHtml(str) {
    if (!str) return '';
    return String(str)
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#39;');
  }

  function escapeAttr(str) {
    if (!str) return '';
    return String(str)
      .replace(/&/g, '&amp;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#39;');
  }

  // Formatowanie MB na czytelna wartosc
  function formatMb(mb) {
    if (mb >= 1024) {
      return (mb / 1024).toFixed(1) + ' GB';
    }
    return mb + ' MB';
  }

  // Formatowanie bajtow/s na czytelna wartosc
  function formatBytes(bytes) {
    if (bytes < 1024) return bytes + ' B/s';
    if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + ' KB/s';
    if (bytes < 1024 * 1024 * 1024) return (bytes / (1024 * 1024)).toFixed(1) + ' MB/s';
    return (bytes / (1024 * 1024 * 1024)).toFixed(1) + ' GB/s';
  }

  // Formatowanie daty z lokalizacja
  function formatDate(dateStr) {
    if (!dateStr) return '-';
    try {
      const locale = I18n.getLanguage() === 'pl' ? 'pl-PL' : 'en-US';
      return new Date(dateStr).toLocaleDateString(locale, {
        day: '2-digit', month: '2-digit', year: 'numeric',
        hour: '2-digit', minute: '2-digit',
      });
    } catch {
      return dateStr;
    }
  }

  return { escapeHtml, escapeAttr, formatMb, formatBytes, formatDate };
})();
