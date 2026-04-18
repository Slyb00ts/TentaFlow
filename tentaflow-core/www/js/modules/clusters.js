// =============================================================================
// Plik: modules/clusters.js
// Opis: Edycja klastra (W-UPDATE).
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, toast } from '/js/utils.js';

const ClustersScreen = {
  title: 'Klastry',
  render() {
    return `
      <div class="content-header"><h1>Klastry</h1></div>
      <div class="card">
        <h3 class="card-title" style="margin-bottom: var(--space-4);">Edytuj cluster</h3>
        <div class="form-row"><label class="label" for="cl-id">Cluster ID</label>
          <input class="input" id="cl-id" placeholder="dev"></div>
        <div class="form-row"><label class="label" for="cl-name">Nazwa</label>
          <input class="input" id="cl-name"></div>
        <div class="form-row"><label class="label" for="cl-desc">Opis</label>
          <textarea class="textarea" id="cl-desc"></textarea></div>
        <button class="btn btn-primary" id="cl-save">Zapisz zmiany</button>
        <p style="margin-top: var(--space-4); color: var(--color-text-muted); font-size: var(--text-sm);">
          Pełna lista klastrów + tworzenie/usuwanie wymaga osobnych wariantów (ClusterListRequest itd.) — dokładamy w kolejnym kroku.
        </p>
      </div>`;
  },
  mount() {
    byId('cl-save').addEventListener('click', async () => {
      const clusterId = byId('cl-id').value.trim();
      const name = byId('cl-name').value.trim();
      const description = byId('cl-desc').value.trim();
      if (!clusterId || !name) { toast('cluster_id i nazwa wymagane', 'warning'); return; }
      try {
        await ApiBinary.action('clusterUpdateRequest', { clusterId, name, description: description || null });
        toast('Zaktualizowano', 'success');
      } catch (err) { toast(`Błąd: ${err.message}`, 'error'); }
    });
  },
  unmount() {},
};

export default ClustersScreen;
