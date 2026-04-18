// =============================================================================
// Plik: modules/clusters/ClustersBinary.js
// Opis: Clusters ekran (W-UPDATE archetyp) zmigrowany na binary protocol.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

const ClustersBinary = (() => {
  'use strict';

  async function updateCluster(clusterId, name, description) {
    try {
      const result = await ApiBinary.action('clusterUpdateRequest', {
        clusterId,
        name,
        description,
      });
      App.showToast(I18n.t('clusters.update_success'), 'success');
      return result;
    } catch (err) {
      App.showToast(`${I18n.t('common.error')}: ${err.message}`, 'error');
      throw err;
    }
  }

  return {
    mount: () => {
      document.getElementById('btn-update-cluster')?.addEventListener('click', () => {
        const clusterId = document.getElementById('cluster-id')?.value;
        const name = document.getElementById('cluster-name')?.value;
        const description = document.getElementById('cluster-description')?.value;
        if (clusterId && name) updateCluster(clusterId, name, description || null);
      });
    },
    unmount: () => {},
    updateCluster,
  };
})();

export default ClustersBinary;
