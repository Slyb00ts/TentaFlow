// =============================================================================
// File: modules/agent-accounts-i18n.test.js
// Description: Every string the agent-account screens render has to exist in
// all five locales with the same interpolation placeholders, and every key the
// three modules ask for has to exist at all — a missing key surfaces as a raw
// `agent_accounts.foo` in the table header, which no key-count check catches.
// The `agents.*` keys of the G01 section and the one `my_accounts` heading are
// verified the same way, one by one.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readdirSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const WWW_ROOT = resolve(HERE, '..', '..');
const LOCALES = ['pl', 'en', 'de', 'es', 'fr'];
const NAMESPACE = 'agent_accounts';

// The `agents.*` keys the CLI runtime section (G01) renders, and the heading
// "Moje konta" grew when the agent-application section landed next to it.
const SINGLE_KEYS = [
  'agents.runtime_kind_llm',
  'agents.runtime_kind_cli',
  'agents.runtime_kind_hint',
  'agents.label_cli_engine',
  'agents.cli_engine_pick',
  'agents.cli_model_default',
  'agents.cli_model_hint',
  'agents.cli_temperature_hint',
  'agents.label_cli_account',
  'agents.cli_account_global',
  'agents.cli_account_global_desc',
  'agents.cli_account_user',
  'agents.cli_account_user_desc',
  'agents.cli_account_hint',
  'agents.cli_account_pick',
  'agents.cli_account_none',
  'agents.err_cli_engine_required',
  'agents.err_cli_account_required',
  'agents.cli_reasoning.minimal',
  'agents.cli_reasoning.standard',
  'agents.cli_reasoning.maximal',
  'my_accounts.section_addons',
];

// Keys that print a number followed by the thing it counts. Every one of their
// variables has to carry its own plural selector — a fixed noun after a count
// prints "1 kont" or "1 compartidas" in whatever language inflects it, which is
// exactly how the account footer shipped its first version.
const COUNTER_KEYS = [
  'list_foot', 'access_foot', 'agents_foot', 'access_members', 'node_accounts', 'apps_sessions',
];

// Every key the four modules pass to `T()`, including the ones chosen by a
// ternary or by the status map, listed here so a rename in the module without
// a rename in the bundles fails the suite instead of the screen.
const USED_KEYS = [
  'access_add_placeholder', 'access_empty', 'access_foot', 'access_foot_org',
  'access_group_groups', 'access_group_users', 'access_members', 'access_mode_org',
  'access_mode_selected', 'access_note', 'access_save', 'access_saved', 'access_whole_org',
  'access_whole_org_sub', 'action_create', 'action_delete', 'action_end_session',
  'action_key_clear', 'action_key_save', 'action_login', 'action_open', 'action_relogin',
  'action_rename', 'action_revoke', 'agents_empty', 'agents_foot', 'agents_note',
  'apps_card_sub', 'apps_connect', 'apps_connect_hint', 'apps_disconnect',
  'apps_disconnect_confirm_body', 'apps_disconnect_confirm_title', 'apps_disconnected',
  'apps_empty', 'apps_last_used', 'apps_never_used', 'apps_not_connected', 'apps_section',
  'apps_sessions', 'apps_shared', 'apps_used_on', 'bind_mode_global', 'bind_mode_user',
  'col_account', 'col_active_sessions', 'col_agent', 'col_bind_mode', 'col_credential',
  'col_granted_by', 'col_kind', 'col_node', 'col_node_credential', 'col_node_sessions',
  'col_receives', 'col_sandbox', 'col_scope', 'col_sessions', 'col_since', 'col_status',
  'col_used_on', 'col_user', 'col_who', 'col_workspace', 'create_login_note',
  'create_no_engines', 'create_ok', 'create_sub', 'create_sub_own', 'create_submit',
  'create_title', 'create_title_own', 'credential_api_key', 'credential_subscription',
  'delete_confirm_body', 'delete_confirm_hint', 'delete_confirm_title', 'deleted',
  'err_key_required', 'err_name_required', 'error_timeout', 'error_unknown', 'field_api_key',
  'field_api_key_hint', 'field_api_key_placeholder', 'field_api_key_replace', 'field_enabled',
  'field_enabled_hint', 'field_engine', 'field_name', 'field_name_placeholder', 'filter_engine',
  'filter_engine_all', 'filter_scope', 'filter_scope_all', 'install_absent', 'install_error',
  'install_installed', 'install_installing', 'key_clear_confirm_body',
  'key_clear_confirm_title', 'key_cleared', 'key_saved', 'kind_hint_api_key', 'kind_hint_login',
  'kv_credential', 'kv_engine', 'kv_needs_key_hint', 'kv_needs_login_hint', 'kv_no_credential',
  'kv_provider_account', 'kv_revision', 'kv_status', 'list_empty', 'list_foot',
  'login.account_disabled', 'login.action_cancel', 'login.action_copy', 'login.action_open',
  'login.action_start', 'login.action_submit', 'login.already_running', 'login.copied',
  'login.copy_failed', 'login.engine_missing', 'login.field_code_placeholder',
  'login.field_node', 'login.lost', 'login.no_address_closed', 'login.no_address_failed',
  'login.no_address_timeout', 'login.no_terminal', 'login.node_not_receiving',
  'login.not_a_login_account', 'login.owner_only', 'login.result_cancelled',
  'login.result_failed', 'login.result_ok', 'login.result_ok_as', 'login.result_starting',
  'login.result_verifying', 'login.step_code', 'login.step_open', 'login.step_open_hint',
  'login.step_result', 'login.step_result_hint', 'login.step_start', 'login.step_start_hint',
  'login.title', 'node_accounts', 'node_accounts_none', 'node_offline', 'node_online',
  'node_state_current', 'node_state_error', 'node_state_materializing',
  'node_state_on_first_use', 'node_state_stale', 'node_unnamed', 'node_local', 'nodes_empty',
  'nodes_title',
  'own_account',
  'owner_only', 'receives_off', 'receives_on', 'renamed', 'runtime_actions', 'runtime_empty',
  'runtime_foot_install', 'runtime_foot_receives', 'runtime_install', 'runtime_install_started',
  'runtime_installed', 'runtime_node_unreachable', 'runtime_not_possible', 'runtime_os_linux',
  'runtime_os_macos', 'runtime_os_unknown', 'runtime_os_windows', 'runtime_reinstall',
  'runtime_uninstall', 'runtime_uninstall_started', 'runtime_uninstalled', 'sandbox_available',
  'sandbox_missing', 'sandbox_unknown', 'scope_global', 'scope_user', 'search_placeholder',
  'segment_accounts', 'segment_runtime', 'session_end_confirm_body',
  'session_end_confirm_title', 'session_ended', 'sessions_empty', 'sessions_hint',
  'sessions_home_lost', 'sessions_no_data', 'sessions_remote_empty', 'sessions_remote_home',
  'sessions_remote_offline', 'sessions_scope_unknown', 'sessions_scope_unknown_note',
  'sessions_title', 'sessions_title_scope', 'settings_title', 'since_hours', 'since_minutes',
  'status_active_any',
  'status_active_api_key', 'status_active_login', 'status_disabled', 'status_needs_any',
  'status_needs_key', 'status_needs_login', 'status_pending_any', 'status_pending_api_key',
  'status_pending_login', 'status_unknown', 'subject_group', 'subject_org', 'subject_user',
  'subtitle_global', 'subtitle_user', 'tab', 'tab_access', 'tab_agents', 'tab_overview',
  'title', 'used_on_tooltip', 'value_none',
];

const bundles = Object.fromEntries(LOCALES.map((locale) => [
  locale,
  JSON.parse(readFileSync(join(WWW_ROOT, 'i18n', `${locale}.json`), 'utf8')),
]));
const dig = (obj, path) => path.split('.').reduce((o, k) => (o && typeof o === 'object' ? o[k] : undefined), obj);
const flatten = (obj, prefix = '') => Object.entries(obj)
  .flatMap(([k, v]) => (v && typeof v === 'object' ? flatten(v, `${prefix}${k}.`) : [`${prefix}${k}`]));
// `{n}` and the plural selector `{n|one|few|many}` both name the parameter first.
const placeholders = (s) => [...String(s).matchAll(/\{([a-zA-Z0-9_]+)(?:\|[^}]*)?\}/g)].map((m) => m[1]).sort();

const reference = flatten(dig(bundles.pl, NAMESPACE)).sort();

test('the whole agent_accounts namespace has the same key set in all five locales', () => {
  assert.ok(reference.length > 0, 'the pl namespace is not empty');
  for (const locale of LOCALES) {
    assert.deepEqual(flatten(dig(bundles[locale], NAMESPACE) || {}).sort(), reference, `${NAMESPACE} keys in ${locale}`);
  }
});

test('every key the agent-account modules render exists in every locale', () => {
  for (const key of USED_KEYS) {
    for (const locale of LOCALES) {
      const value = dig(bundles[locale], `${NAMESPACE}.${key}`);
      assert.equal(typeof value, 'string', `${NAMESPACE}.${key} in ${locale}`);
      assert.ok(value.trim().length > 0, `${NAMESPACE}.${key} in ${locale} is not blank`);
    }
  }
});

// Core answers a sign-in, a credential write and a runtime refusal with a
// message KEY, not a sentence: the dashboard renders it in the operator's
// language. A key that only exists in the Rust source prints as
// `agent_accounts.login.failed` on the screen, which is the same defect as a
// missing translation and is caught by nothing else — the Rust side never reads
// the bundles.
function rustSources(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return rustSources(path);
    return entry.isFile() && entry.name.endsWith('.rs') ? [path] : [];
  });
}

test('every message key Core emits exists in every locale', () => {
  const emitted = new Set();
  for (const file of rustSources(resolve(WWW_ROOT, '..', 'src'))) {
    for (const [, key] of readFileSync(file, 'utf8').matchAll(/"(agent_accounts\.[a-z0-9_.]+)"/g)) {
      emitted.add(key);
    }
  }
  assert.ok(emitted.size > 0, 'Core emits agent_accounts keys');
  for (const key of [...emitted].sort()) {
    for (const locale of LOCALES) {
      const value = dig(bundles[locale], key);
      assert.equal(typeof value, 'string', `${key} in ${locale}`);
      assert.ok(value.trim().length > 0, `${key} in ${locale} is not blank`);
    }
  }
});

test('the G01 runtime keys and the "Moje konta" heading exist in every locale', () => {
  for (const key of SINGLE_KEYS) {
    for (const locale of LOCALES) {
      const value = dig(bundles[locale], key);
      assert.equal(typeof value, 'string', `${key} in ${locale}`);
      assert.ok(value.trim().length > 0, `${key} in ${locale} is not blank`);
    }
  }
});

test('interpolation placeholders match the Polish source in every locale', () => {
  const keys = [...reference.map((k) => `${NAMESPACE}.${k}`), ...SINGLE_KEYS];
  for (const key of keys) {
    const expected = placeholders(dig(bundles.pl, key));
    for (const locale of LOCALES) {
      assert.deepEqual(placeholders(dig(bundles[locale], key)), expected, `${key} placeholders in ${locale}`);
    }
  }
});

test('a counted noun is inflected for every number, in every locale', () => {
  for (const key of COUNTER_KEYS) {
    for (const locale of LOCALES) {
      const value = String(dig(bundles[locale], `${NAMESPACE}.${key}`));
      // Plain `{var}` occurrences only: a selector names its own variable too.
      // The inflected word may sit a word or two later ("2 access entries"),
      // but it has to come BEFORE the next number in the same sentence.
      for (const match of value.matchAll(/\{([a-zA-Z0-9_]+)\}/g)) {
        const rest = value.slice(match.index + match[0].length);
        const next = [...rest.matchAll(/\{([a-zA-Z0-9_]+)(\|[^}]*)?\}/g)][0];
        assert.ok(
          next && next[1] === match[1] && next[2],
          `${NAMESPACE}.${key} in ${locale}: {${match[1]}} counts a fixed word instead of its plural forms`,
        );
      }
    }
  }
});

// Polish needs three plural forms; the other four languages need two. A list
// with one form is a concatenation waiting to print "1 kont".
test('every plural selector carries the forms its language needs', () => {
  const keys = [...reference.map((k) => `${NAMESPACE}.${k}`), ...SINGLE_KEYS];
  for (const key of keys) {
    for (const locale of LOCALES) {
      const value = String(dig(bundles[locale], key));
      for (const [, forms] of value.matchAll(/\{[a-zA-Z0-9_]+\|([^}]*)\}/g)) {
        const count = forms.split('|').length;
        const needed = locale === 'pl' ? 3 : 2;
        assert.ok(count >= needed, `${key} in ${locale} has ${count} plural forms, needs ${needed}`);
      }
    }
  }
});
