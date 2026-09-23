// =============================================================================
// File: modules/addons/robot-cloud-dialog.js
// Description: "Sign in to the robot vendor account" window shared by the addon
//              install form and the addon settings tab. The admin enters the
//              vendor account (Unitree today), Core logs in, lists the robots
//              bound to it (serial + per-device key) and runs LAN discovery for
//              their current IP; the admin picks one and the caller fills its
//              own form fields. The password is sent once and never kept here.
// Example: openRobotCloudDialog({ provider: 'unitree', onPick: (d) => fill(d) });
// =============================================================================

import '/js/components/tf-window.js';
import '/js/components/tf-radio.js';
import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';

const t = (key, params) => I18n.t(`addons.robot_cloud.${key}`, params);

/** Vendor display names; the provider id itself comes from the package manifest. */
const PROVIDER_NAMES = { unitree: 'Unitree' };

export function robotCloudVendorName(provider) {
  return PROVIDER_NAMES[provider] || provider;
}

/**
 * Connection-param values a picked robot provides, keyed like the package's
 * `[[robot.connection_param]]` keys. Only non-empty values are returned, so a
 * robot that did not answer discovery never blanks an IP typed by hand.
 */
export function robotCloudFieldValues(device) {
  const out = {};
  if (device.serial) out.serial = device.serial;
  const key = device.aesKey ?? device.aes_key ?? '';
  if (key) out.aes_key = key;
  const ip = device.lanIp ?? device.lan_ip ?? '';
  if (ip) out.ip = ip;
  return out;
}

export function openRobotCloudDialog({ provider, onPick }) {
  const vendor = robotCloudVendorName(provider);
  const win = document.createElement('tf-window');
  win.setAttribute('title', t('title', { vendor }));
  win.setAttribute('icon', 'cloud');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('min-width', '420');
  win.setAttribute('width', '480');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');

  const body = document.createElement('div');
  body.slot = 'body';
  const foot = document.createElement('div');
  foot.slot = 'footer';
  win.appendChild(body);
  win.appendChild(foot);

  let devices = [];
  let busy = false;

  const renderLogin = () => {
    body.innerHTML = `
      <div style="display:flex;flex-direction:column;gap:12px;font-size:13px;">
        <div style="color:var(--text-2);">${escapeHtml(t('intro', { vendor }))}</div>
        <tf-input id="rc-email" type="email" autocomplete="username" label="${escapeAttr(t('email'))}" required></tf-input>
        <tf-input id="rc-password" type="password" autocomplete="current-password" label="${escapeAttr(t('password'))}" required></tf-input>
        <tf-select id="rc-region" label="${escapeAttr(t('region'))}" value="global">
          <option value="global">${escapeHtml(t('region_global'))}</option>
          <option value="cn">${escapeHtml(t('region_cn'))}</option>
        </tf-select>
        <div style="color:var(--text-3);font-size:12px;">${escapeHtml(t('privacy'))}</div>
      </div>`;
    foot.innerHTML = `
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="primary" icon="download" data-action="confirm">${escapeHtml(t('fetch'))}</tf-button>`;
  };

  const renderDevices = () => {
    const radios = devices.map((d, i) => {
      const name = d.alias || d.model || d.serial;
      const key = d.aesKey ?? d.aes_key ?? '';
      const ip = d.lanIp ?? d.lan_ip ?? '';
      const hint = [
        `${t('serial')}: ${d.serial}`,
        key ? t('key_present') : t('key_absent'),
        ip ? `${t('ip')}: ${ip}` : t('ip_not_found'),
      ].join(' · ');
      return `<tf-radio value="${i}" label="${escapeAttr(name)}" hint="${escapeAttr(hint)}"></tf-radio>`;
    }).join('');
    body.innerHTML = `
      <div style="display:flex;flex-direction:column;gap:12px;font-size:13px;">
        <div style="color:var(--text-2);">${escapeHtml(t('pick', { count: devices.length }))}</div>
        <tf-radio-group id="rc-device" name="rc-device" value="0">${radios}</tf-radio-group>
        ${devices.some((d) => !(d.lanIp ?? d.lan_ip)) ? `<div style="color:var(--text-3);font-size:12px;">${escapeHtml(t('ip_hint'))}</div>` : ''}
      </div>`;
    foot.innerHTML = `
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="primary" icon="check" data-action="confirm">${escapeHtml(t('use'))}</tf-button>`;
  };

  const fetchDevices = async () => {
    const email = (body.querySelector('#rc-email')?.value || '').trim();
    const password = body.querySelector('#rc-password')?.value || '';
    const region = body.querySelector('#rc-region')?.value || 'global';
    if (!email || !password) {
      toast(t('credentials_required'), 'error');
      return;
    }
    busy = true;
    foot.querySelector('[data-action="confirm"]')?.setAttribute('disabled', '');
    try {
      const res = await ApiBinary.one('robotCloudDevicesRequest', { provider, region, email, password });
      devices = Array.isArray(res.devices) ? res.devices : [];
      if (devices.length === 0) {
        toast(t('no_devices', { vendor }), 'warning');
        return;
      }
      renderDevices();
    } catch (err) {
      toast(`${t('fetch_error')}: ${err.message}`, 'error');
    } finally {
      busy = false;
      foot.querySelector('[data-action="confirm"]')?.removeAttribute('disabled');
    }
  };

  win.addEventListener('action', async (e) => {
    if (e.detail?.action !== 'confirm') return;
    e.preventDefault();
    if (busy) return;
    if (devices.length === 0) {
      await fetchDevices();
      return;
    }
    const index = Number(body.querySelector('#rc-device')?.value ?? 0);
    const device = devices[index];
    if (!device) return;
    onPick(device);
    win.close(true);
  });

  renderLogin();
  document.body.appendChild(win);
  return win;
}
