// =============================================================================
// Plik: MeshNetworkConfig.js
// Opis: Modal konfiguracji sieci — DHCP/Static toggle, walidacja IP,
//       zdalne wykonanie przez MeshCommand.
// =============================================================================

const MeshNetworkConfig = (() => {
  'use strict';

  const APPLY_TIMEOUT_MS = 30000;

  // Walidacja adresu IPv4
  function isValidIpv4(ip) {
    const re = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/;
    const m = re.exec(ip);
    if (!m) return false;
    for (let i = 1; i <= 4; i++) {
      const octet = parseInt(m[i], 10);
      if (octet < 0 || octet > 255) return false;
    }
    return true;
  }

  // Konwersja CIDR na maska kropkowa (np. /24 -> 255.255.255.0)
  function cidrToMask(cidr) {
    const bits = parseInt(cidr.replace('/', ''), 10);
    if (isNaN(bits) || bits < 0 || bits > 32) return null;
    const mask = bits === 0 ? 0 : (~0 << (32 - bits)) >>> 0;
    return [
      (mask >>> 24) & 0xff,
      (mask >>> 16) & 0xff,
      (mask >>> 8) & 0xff,
      mask & 0xff
    ].join('.');
  }

  // Walidacja maski — akceptuje kropkowa lub CIDR, zwraca kropkowa
  function parseNetmask(value) {
    if (!value) return null;
    const trimmed = value.trim();
    if (trimmed.startsWith('/') || /^\d{1,2}$/.test(trimmed)) {
      const cidr = trimmed.startsWith('/') ? trimmed : '/' + trimmed;
      return cidrToMask(cidr);
    }
    if (isValidIpv4(trimmed)) return trimmed;
    return null;
  }

  // Konwersja IP na 32-bitowa liczbe
  function ipToInt(ip) {
    const parts = ip.split('.').map(Number);
    return ((parts[0] << 24) | (parts[1] << 16) | (parts[2] << 8) | parts[3]) >>> 0;
  }

  // Sprawdzenie czy gateway jest w tej samej podsieci
  function isInSameSubnet(ip, gateway, mask) {
    const ipInt = ipToInt(ip);
    const gwInt = ipToInt(gateway);
    const maskInt = ipToInt(mask);
    return (ipInt & maskInt) === (gwInt & maskInt);
  }

  // Walidacja pol formularza — zwraca obiekt bledow per pole
  function validateFields(isDhcp, ipv4, netmask, gateway, sudoPassword) {
    const errors = {};
    if (!sudoPassword || !sudoPassword.trim()) {
      errors.sudoPassword = I18n.t('mesh.network_password_required');
    }
    if (isDhcp) return errors;

    if (!ipv4 || !isValidIpv4(ipv4.trim())) {
      errors.ipv4 = I18n.t('mesh.network_invalid_ip');
    }

    const parsedMask = parseNetmask(netmask);
    if (!parsedMask) {
      errors.netmask = I18n.t('mesh.network_invalid_mask');
    }

    if (!gateway || !isValidIpv4(gateway.trim())) {
      errors.gateway = I18n.t('mesh.network_invalid_gateway');
    } else if (parsedMask && ipv4 && isValidIpv4(ipv4.trim()) && !isInSameSubnet(ipv4.trim(), gateway.trim(), parsedMask)) {
      errors.gateway = I18n.t('mesh.network_gateway_subnet');
    }

    return errors;
  }

  // Wyswietlenie bledow walidacji pod polami
  function showFieldErrors(overlay, errors) {
    overlay.querySelectorAll('.mesh-network-field-error').forEach(el => {
      el.textContent = '';
    });
    for (const [field, msg] of Object.entries(errors)) {
      const el = overlay.querySelector(`[data-error-for="${field}"]`);
      if (el) el.textContent = msg;
    }
  }

  // Przelaczenie trybu DHCP/Static — wlacz/wylacz pola
  function toggleMode(overlay, isDhcp) {
    const fields = overlay.querySelectorAll('.mesh-network-static-field');
    fields.forEach(field => {
      if (isDhcp) {
        field.classList.add('mesh-network-field-disabled');
        const input = field.querySelector('input');
        if (input) input.disabled = true;
      } else {
        field.classList.remove('mesh-network-field-disabled');
        const input = field.querySelector('input');
        if (input) input.disabled = false;
      }
    });

    overlay.querySelectorAll('.mesh-network-mode-btn').forEach(btn => {
      btn.classList.toggle('active', btn.dataset.mode === (isDhcp ? 'dhcp' : 'static'));
    });
  }

  // Otwarcie modala konfiguracji sieci
  function show(nodeId, interfaceName, interfaceData) {
    const title = I18n.t('mesh.network_config_title').replace('{name}', Utils.escapeHtml(interfaceName));
    const currentIp = (interfaceData && interfaceData.ipv4_address) || '';
    const currentMask = (interfaceData && interfaceData.ipv4_netmask) || '';
    const currentGateway = (interfaceData && interfaceData.ipv4_gateway) || '';

    const overlay = document.createElement('div');
    overlay.className = 'modal-overlay active';
    overlay.innerHTML = `
      <div class="modal" style="max-width:520px;">
        <div class="modal-header">
          <h3>${title}</h3>
          <button class="modal-close" data-action="close">&times;</button>
        </div>
        <div class="modal-body">
          <div class="form-group">
            <label class="form-label">${I18n.t('mesh.network_mode_dhcp')} / ${I18n.t('mesh.network_mode_static')}</label>
            <div class="mesh-network-mode-toggle">
              <button type="button" class="btn btn-sm mesh-network-mode-btn active" data-mode="dhcp">${I18n.t('mesh.network_mode_dhcp')}</button>
              <button type="button" class="btn btn-sm mesh-network-mode-btn" data-mode="static">${I18n.t('mesh.network_mode_static')}</button>
            </div>
          </div>

          <div class="form-group mesh-network-static-field mesh-network-field-disabled">
            <label>${I18n.t('mesh.network_ipv4')}</label>
            <input type="text" id="net-cfg-ipv4" placeholder="192.168.1.10" value="${Utils.escapeAttr(currentIp)}" disabled>
            <div class="mesh-network-field-error" data-error-for="ipv4"></div>
          </div>

          <div class="form-group mesh-network-static-field mesh-network-field-disabled">
            <label>${I18n.t('mesh.network_netmask')}</label>
            <input type="text" id="net-cfg-netmask" placeholder="255.255.255.0 or /24" value="${Utils.escapeAttr(currentMask)}" disabled>
            <div class="mesh-network-field-error" data-error-for="netmask"></div>
          </div>

          <div class="form-group mesh-network-static-field mesh-network-field-disabled">
            <label>${I18n.t('mesh.network_gateway')}</label>
            <input type="text" id="net-cfg-gateway" placeholder="192.168.1.1" value="${Utils.escapeAttr(currentGateway)}" disabled>
            <div class="mesh-network-field-error" data-error-for="gateway"></div>
          </div>

          <div class="form-group">
            <label>${I18n.t('mesh.network_sudo_password')}</label>
            <input type="password" id="net-cfg-sudo" placeholder="********" autocomplete="off">
            <div class="mesh-network-field-error" data-error-for="sudoPassword"></div>
          </div>
        </div>
        <div class="modal-footer">
          <button class="btn btn-secondary" data-action="close">${I18n.t('common.cancel')}</button>
          <button class="btn btn-primary" data-action="apply" id="net-cfg-apply">${I18n.t('mesh.network_apply')}</button>
        </div>
      </div>
    `;

    document.body.appendChild(overlay);

    let isDhcp = true;

    // Obsluga przelaczania DHCP/Static
    overlay.querySelectorAll('.mesh-network-mode-btn').forEach(btn => {
      btn.addEventListener('click', () => {
        isDhcp = btn.dataset.mode === 'dhcp';
        toggleMode(overlay, isDhcp);
        showFieldErrors(overlay, {});
      });
    });

    // Zamykanie modala — wyczysc haslo przed usunieciem DOM
    const closeModal = () => {
      const sudoInput = overlay.querySelector('#net-cfg-sudo');
      if (sudoInput) {
        sudoInput.value = '';
        sudoInput.setAttribute('value', '');
      }
      if (overlay.parentNode) overlay.remove();
    };
    overlay.querySelectorAll('[data-action="close"]').forEach(btn => {
      btn.addEventListener('click', closeModal);
    });
    overlay.addEventListener('click', (e) => {
      if (e.target === overlay) closeModal();
    });

    // Wyczysc pole hasla i wszystkie referencje
    function clearSensitiveData(sudoInput) {
      sudoInput.value = '';
      sudoInput.setAttribute('value', '');
    }

    // Wyslanie konfiguracji
    const applyBtn = overlay.querySelector('#net-cfg-apply');
    applyBtn.addEventListener('click', async () => {
      const sudoInput = overlay.querySelector('#net-cfg-sudo');
      const ipv4 = overlay.querySelector('#net-cfg-ipv4').value;
      const netmaskRaw = overlay.querySelector('#net-cfg-netmask').value;
      const gateway = overlay.querySelector('#net-cfg-gateway').value;
      const sudoPassword = sudoInput.value;

      const errors = validateFields(isDhcp, ipv4, netmaskRaw, gateway, sudoPassword);
      showFieldErrors(overlay, errors);
      if (Object.keys(errors).length > 0) return;

      const parsedMask = isDhcp ? null : parseNetmask(netmaskRaw);

      applyBtn.disabled = true;
      applyBtn.textContent = I18n.t('mesh.network_applying');

      const payload = {
        interface: interfaceName,
        dhcp: isDhcp,
        ipv4: isDhcp ? null : ipv4.trim(),
        netmask: isDhcp ? null : parsedMask,
        gateway: isDhcp ? null : gateway.trim(),
        sudo_password: sudoPassword
      };

      // Wyczysc haslo z DOM natychmiast po skopiowaniu do payloadu
      clearSensitiveData(sudoInput);

      const controller = new AbortController();
      const timeout = setTimeout(() => controller.abort(), APPLY_TIMEOUT_MS);

      try {
        await ApiClient.post(
          `/api/mesh/nodes/${encodeURIComponent(nodeId)}/network-config`,
          payload,
          { signal: controller.signal }
        );
        clearTimeout(timeout);
        // Wyczysc payload z haslem
        payload.sudo_password = '';

        if (!isDhcp && currentIp && ipv4.trim() !== currentIp) {
          App.showToast(I18n.t('mesh.network_ip_changed_warning'), 'info');
        }
        App.showToast(isDhcp ? I18n.t('mesh.network_success_dhcp') : I18n.t('mesh.network_success'), 'success');
        closeModal();
      } catch (err) {
        clearTimeout(timeout);
        payload.sudo_password = '';
        applyBtn.disabled = false;
        applyBtn.textContent = I18n.t('mesh.network_apply');

        if (err.name === 'AbortError') {
          App.showToast(I18n.t('mesh.network_timeout'), 'error');
        } else if (err.message && err.message.toLowerCase().includes('sudo')) {
          App.showToast(I18n.t('mesh.network_sudo_fail'), 'error');
        } else {
          App.showToast(err.message || I18n.t('common.error'), 'error');
        }
      }
    });
  }

  return { show };
})();
