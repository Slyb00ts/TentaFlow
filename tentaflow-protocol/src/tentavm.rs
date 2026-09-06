// =============================================================================
// File: tentavm.rs
// Purpose: Binary CBOR protocol for TentaVM — the virtualization app. Phase 0
//          covers what the dashboard needs before a single machine exists:
//          the environment summary (P01), the host list and one host (H01/H02),
//          the environment probe that decides whether an engine can run at all
//          (H04), the job list, one job and its cancellation (Z01), per-host
//          grants (H06), the environment settings, and the two writes the P01
//          inbox needs to stay usable: filing an access request and deferring
//          an item. Every request carries `instance_id` — one node
//          runs several TentaVM environments side by side, and the row sets of
//          machines, jobs, grants and settings are partitioned by it. Requests
//          that touch a specific host also carry `host_id`; the dispatcher
//          routes them to the node that owns that host (`route_to_owner`), so
//          no payload names a node.
// Example: MessageBody::TentaVmBody(TentaVmPayload::HostsListRequest {
//              instance_id: "vm-default".to_string(),
//          })
// =============================================================================

use serde::{Deserialize, Serialize};

use crate::features::FeatureState;

// =============================================================================
// Text
// =============================================================================

/// One substitution for a `VmText`. `name` is the placeholder as it appears in
/// the translation ("host", "count", "image"), `value` is already-formatted
/// data — a host name, a number, a version — never a translated word.
///
/// Two things the renderer has to get right, because `www/js/i18n.js` is
/// stricter than this shape looks. `I18n.t(key, vars)` takes an OBJECT and
/// tests `key in vars`, so a list has to be turned into one
/// (`Object.fromEntries(params.map(p => [p.name, p.value]))`) or every
/// placeholder is printed raw. And the plural selector `{count|a|b|c}` runs
/// `Math.abs(Number(value))`: a numeric string like "3" is correct, but a
/// non-numeric value silently picks the LAST form and an empty string counts
/// as zero. A count therefore has to arrive as digits, never as "" and never
/// as "kilka".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VmTextParam {
    pub name: String,
    pub value: String,
}

/// A sentence the node composes but does NOT translate: `key` names the i18n
/// entry under the app's `tentavm` namespace and `params` fills its
/// placeholders. Every human-readable string this module produces travels this
/// way, so the wire never freezes one language — the same rule that removed a
/// stored `title` from `vm_jobs`.
///
/// An empty `key` means "no text"; a reader must render nothing, not the empty
/// string as a label. The exceptions are documented at their own fields:
/// version strings, engine diagnostics and package names are DATA and travel
/// verbatim as `String`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VmText {
    pub key: String,
    pub params: Vec<VmTextParam>,
}

// =============================================================================
// Hosts
// =============================================================================

/// One engine on a host. `id` is the driver key of the platform matrix
/// (§5.1): 'kvm' | 'incus' | 'podman' | 'docker' | 'kubernetes' | 'hyperv' |
/// 'vz' | 'qemu_hvf' | 'parallels' | 'apple_container' | 'proxmox' |
/// 'vsphere'. `status` is 'ready' (usable now) | 'needs_install' (the packages
/// of its feature are missing) | 'needs_consent' (installed, but the admin has
/// not accepted its root-equivalence yet) | 'disabled' | 'unsupported' (the
/// platform cannot run it) | 'error'. `kinds` says what it runs: 'vm' |
/// 'container' | 'kubernetes'.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VmEngine {
    pub id: String,
    pub status: String,
    pub version: Option<String>,
    pub kinds: Vec<String>,
    /// Versions and package names behind `status` ("libvirt 10.0.0, QEMU
    /// 8.2.2"), shown verbatim on the engine chip. DATA, not prose: there is
    /// nothing here to translate, so it is not a `VmText`.
    pub detail: String,
    /// The engine grants root-equivalence to whoever may use it (kvm, docker),
    /// so enabling it is a separate admin decision (§17.5, dialog D01).
    pub consent_required: bool,
    pub consent_granted: bool,
}

/// One capability of a host or a machine, resolved by
/// `effective_capabilities` (§5.2) and mapped to a UI action by §5.4.
/// `id` is 'live_migrate' | 'migrate_offline' | 'snapshot_disk' |
/// 'snapshot_memory' | 'snapshot_revert' | 'save_restore' | 'console_vnc' |
/// 'console_serial' | 'gpu_passthrough' | 'exec' | 'stats' | 'clone' |
/// 'template'. When `supported` is false the UI keeps the action visible and
/// disabled with `reason` as its explanation — never silently missing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VmCapability {
    pub id: String,
    pub supported: bool,
    pub reason: VmText,
}

/// One host row of the registry (`vm_hosts`, §4.1) joined with what the mesh
/// knows about the node behind it. `kind` is 'node' (a mesh node running
/// TentaVM) | 'connector_host' (a host reached through an external connector).
/// `status` is 'ready' | 'needs_install' | 'maintenance' | 'unreachable' |
/// 'unsupported' | 'unknown'. `your_role` is the caller's own grant on this
/// host — '' | 'view' | 'deploy' | 'manage' (§7.1) — and drives the card's
/// "Twoje uprawnienia" bar as well as which actions the card offers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct VmHost {
    pub host_id: String,
    pub kind: String,
    pub node_id: Option<String>,
    pub connector_id: Option<String>,
    pub external_ref: Option<String>,
    pub display_name: String,
    pub status: String,
    /// Why the host is in that status — the text G01 and the host card show,
    /// as a key plus its data ("host.unreachable" + `last_seen`).
    pub status_reason: VmText,
    pub online: bool,
    pub is_local: bool,
    pub owner_node_id: String,
    pub owner_epoch: u64,
    /// The nine hardware fields below come from the environment probe of the
    /// host they describe (§8.1), and a probe runs on the machine it measures.
    /// A node therefore fills them for ITSELF and leaves them at zero for
    /// every other host, until the two mechanisms that carry the facts across
    /// the fleet exist: capacity as columns of `vm_hosts` (the shape of a
    /// machine is registry state) and utilization in the mesh heartbeat as
    /// `PeerVirtInfo` (load is telemetry and has no business in the ledger).
    ///
    /// So a reader must treat ZERO as "not measured here", never as "this host
    /// has no memory" — and the same holds for the local host between an
    /// install and its first probe. `status` is what says which: a host that
    /// is not `ready` may legitimately have nothing measured yet. There is no
    /// `measured_at` field, which is also why a node reports capacity from an
    /// expired probe (cores do not change) but drops the three `*_used_*`
    /// numbers back to zero rather than draw an old percentage as current.
    pub os_name: String,
    /// May be empty on a rolling distribution: `/etc/os-release` has no
    /// `VERSION_ID` on Arch/CachyOS, and inventing one would be worse.
    pub os_version: String,
    pub arch: String,
    /// Logical CPUs, not physical cores — this is what an overcommit ratio is
    /// computed against.
    pub cpu_cores: u32,
    pub cpu_used_pct: f64,
    pub ram_bytes: u64,
    /// Total minus AVAILABLE, not minus free: page cache is memory a guest can
    /// still be given, and counting it as used draws every idle Linux host at
    /// nearly full.
    pub ram_used_bytes: u64,
    /// The filesystem the machines' disks land on, not the root one — with one
    /// caveat the reader has to know: the node measures it through an
    /// enumeration that does not list every mount point (measured: 4 of 32 on
    /// one machine), so a pool on its own filesystem can be reported as the
    /// filesystem above it. Fixing that changes the figure for every app on the
    /// node, not just this one.
    pub storage_bytes: u64,
    pub storage_used_bytes: u64,
    pub guests_total: u32,
    pub guests_running: u32,
    pub engines: Vec<VmEngine>,
    pub capabilities: Vec<VmCapability>,
    pub your_role: String,
    pub last_seen_at: Option<String>,
    pub updated_at: Option<String>,
}

// =============================================================================
// Environment probe
// =============================================================================

/// What the probe found out about hardware virtualization on the host — the
/// facts the mesh heartbeat republishes as `PeerVirtInfo` so Mesh can tell
/// whether TentaVM can run there without asking TentaVM. `cpu_flag` is 'vmx' |
/// 'svm' | 'apple_vz' | 'hyperv' | '' (none found).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VmVirtSupport {
    pub hardware_virtualization: bool,
    pub cpu_flag: String,
    pub kvm_device: bool,
    pub nested: bool,
    pub iommu: bool,
    pub iommu_groups: u32,
    /// Resizable BAR and the boot framebuffer decide whether a GPU can be
    /// passed through at all, so the probe reads them once here instead of
    /// re-running privileged reads per device later (§8.1).
    pub rebar: bool,
    pub sysfb: bool,
    /// Shown when `hardware_virtualization` is false — the reason the host
    /// cannot run machines at all, which P00 repeats as its empty state.
    pub detail: VmText,
}

/// The full probe result of one host (§8.1). `package_manager` is 'apt' |
/// 'dnf' | 'pacman' | 'zypper' | 'winget' | 'brew' | '' (unknown — install is
/// disabled). `libvirt_daemon_mode` is 'monolithic' | 'modular' | '' and picks
/// which units the installer enables. `security_module` is 'selinux' |
/// 'apparmor' | 'none'.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VmHostEnvironment {
    /// std::env::consts::OS: 'linux' | 'macos' | 'windows'.
    pub platform: String,
    /// True where the whole driver set is implemented; elsewhere the host runs
    /// the limited mode and the UI says so once.
    pub full_support: bool,
    pub os_name: String,
    pub os_version: String,
    pub kernel: String,
    pub hostname: String,
    pub arch: String,
    pub package_manager: String,
    pub virt: VmVirtSupport,
    pub libvirt_version: Option<String>,
    pub libvirt_daemon_mode: String,
    pub qemu_version: Option<String>,
    pub security_module: String,
    /// The unprivileged `tentavm` account that carries rootless Podman exists
    /// with subuid/subgid and linger enabled (§8.3).
    pub tentavm_account: bool,
    pub watchdog_device: bool,
    /// The `FeatureSpec` table of §8.2 evaluated here — the same wire shape
    /// every app that installs system dependencies uses (`features::FeatureState`).
    /// TentaVM's ids are 'kvm_base' | 'guest_tools' | 'incus' |
    /// 'podman_rootless' | 'docker' | 'nvidia_container_toolkit' | 'k3s' |
    /// 'vfio'.
    pub features: Vec<FeatureState>,
    pub engines: Vec<VmEngine>,
    pub capabilities: Vec<VmCapability>,
    /// Union of the `packages` of every feature that is not 'ok' — the "Do
    /// zainstalowania na <host> (6)" block of M02 and H04.
    pub missing_packages: Vec<String>,
    /// Installing what is missing restarts the TentaFlow service on this host,
    /// so H04 warns before it starts instead of after the socket drops.
    pub requires_service_restart: bool,
    pub probed_at: String,
}

// =============================================================================
// Jobs
// =============================================================================

/// One step of a job's saga (§7.4). `state` is 'pending' | 'running' | 'done'
/// | 'failed' | 'skipped' | 'compensated'.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VmJobStep {
    pub id: String,
    pub label: VmText,
    pub state: String,
    pub progress_pct: u8,
    pub detail: VmText,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

/// One line of a job's log, already through the redaction filter that keeps
/// passwords and tickets out of `vm_job_logs` (§4.2). `level` is 'debug' |
/// 'info' | 'warn' | 'error'.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VmJobLogLine {
    pub at: String,
    pub level: String,
    pub step_id: String,
    pub text: String,
}

/// One row of `vm_jobs` (§4.1) — every field but `label`, `guest_name` and
/// `host_name` is a column of that table. `state` is 'queued' | 'running' |
/// 'waiting_restart' (the saga's `restart_self` step is in flight and the UI
/// resubscribes after the service comes back) | 'succeeded' | 'failed' |
/// 'cancelled'. `cancel_semantics` is 'none' (the job cannot be stopped) |
/// 'cooperative' (it stops at the next step boundary, finished steps stand) |
/// 'compensating' (finished steps are rolled back), which is exactly what
/// `JobCancelRequest` promises for this job — the UI reads the semantics off
/// the row instead of guessing per `kind`.
///
/// `vm_jobs` stores no title, and none is invented here: `label` is a key plus
/// data, composed by the node that owns the job from its own row and
/// `steps_json`. Every `kind` yields a non-empty one, including the two whose
/// subject is neither a machine nor a host:
///
/// | `kind` | `label.key` | `label.params` | from |
/// |---|---|---|---|
/// | `host_probe` | `job.host_probe` | `host` | `vm_hosts.display_name` |
/// | `host_environment_install` | `job.host_environment_install` | `host`, `count` | same + the probe's missing-package count |
/// | `guest_create` | `job.guest_create` | `guest`, `host` | `vm_guests.name`, `vm_hosts.display_name` |
/// | `guest_delete` | `job.guest_delete` | `guest` | `vm_guests.name`, captured into `steps_json` at creation so an expired row does not blank the history |
/// | `migration` | `job.migration` | `guest`, `from`, `to` | both host joins |
/// | `snapshot` | `job.snapshot` | `guest`, `snapshot` | `vm_guests.name` + `steps_json` |
/// | `image_fetch` | `job.image_fetch` | `image` | `steps_json` — step `fetch`, key `image`; `vm_jobs` has no image column |
///
/// Where a source says `steps_json`, the contract is the step's own parameter
/// map: step id `fetch` carries `image`, step id `delete` carries `guest`,
/// step id `snapshot` carries `snapshot`. The owner writes them when it
/// creates the job, so a row deleted later cannot blank a finished job's
/// label.
///
/// `guest_name` and `host_name` stay as separate fields because Z01 renders
/// them as their own two-line cell, not only inside the label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VmJob {
    pub job_id: String,
    pub instance_id: String,
    /// 'host_probe' | 'host_environment_install' | 'guest_create' |
    /// 'guest_delete' | 'migration' | 'snapshot' | 'image_fetch'. Phase 0
    /// produces only the first two; the rest arrive with their drivers.
    pub kind: String,
    /// The row's own title, never empty — see the table above.
    pub label: VmText,
    pub guest_id: Option<String>,
    /// `vm_guests.name` of `guest_id`, joined by the answering node; empty
    /// when the job concerns no machine (or the row is already gone).
    pub guest_name: String,
    pub source_host_id: Option<String>,
    pub target_host_id: Option<String>,
    /// `vm_hosts.display_name` of `target_host_id`, falling back to
    /// `source_host_id`; empty when the job concerns no host.
    pub host_name: String,
    pub owner_node_id: String,
    pub state: String,
    pub progress_pct: u8,
    pub phase: VmText,
    pub steps: Vec<VmJobStep>,
    pub cancel_semantics: String,
    pub resume_after_restart: bool,
    /// What the engine or the package manager actually said, verbatim and
    /// untranslated — a diagnostic to copy into a bug report, not a sentence
    /// for the user, so it is a `String` and not a `VmText`.
    pub error: String,
    pub created_by: String,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

// =============================================================================
// Grants
// =============================================================================

/// One row of `vm_host_grants` (§4.1). `subject_kind` is 'user' | 'group',
/// `role` is 'view' | 'deploy' | 'manage'. The executing node checks these
/// independently of the operator-node assertions of §7.1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VmHostGrant {
    pub host_id: String,
    pub subject_kind: String,
    pub subject_id: String,
    /// Display name of the user or group, so H06 need not join the directory.
    pub subject_label: String,
    pub role: String,
    pub granted_by: String,
    pub granted_at: String,
}

/// One cell the H06 matrix writes back. An empty `role` removes the grant —
/// the matrix always sends the full desired state of the host, never a diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VmHostGrantInput {
    pub subject_kind: String,
    pub subject_id: String,
    pub role: String,
}

/// A user or group the H06 matrix may add a row for, from the org directory
/// the caller is allowed to see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VmGrantCandidate {
    pub subject_kind: String,
    pub subject_id: String,
    pub subject_label: String,
}

// =============================================================================
// Access requests (P00)
// =============================================================================

/// One row of the access-request ledger, as a READER sees it (§17.4, step 6).
///
/// `scope` is 'instance_create' (the caller wants `vm.create` in this
/// environment) | 'host_role' (they want `role` on `host_id`). `role` is '' for
/// `instance_create` and one of 'view' | 'deploy' | 'manage' otherwise.
///
/// `state` is 'pending' | 'approved' | 'rejected' | 'expired' and is COMPUTED,
/// never stored: the store keeps an immutable request row and an append-only
/// set of decision rows, and the state is a function of that set plus the
/// clock. That is what makes two administrators deciding at once converge —
/// a rejection anywhere wins, so every node folds the same answer whatever
/// order the operations arrive in — and it is why a peer can never send this
/// field: there is no column behind it.
///
/// `expires_at` is on the wire deliberately. Without it a browser can show
/// neither "wygasa za" nor the difference between an expired request and a
/// refused one, and it would offer a decision button on a row the server
/// refuses — the decision handler declines a request whose term has passed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VmAccessRequest {
    pub request_id: String,
    pub scope: String,
    pub host_id: Option<String>,
    pub role: String,
    pub reason: String,
    /// The requester's `user_id`, not their login: a login is renameable and a
    /// key computed from one stops being stable. No foreign key, like every
    /// other subject column of this registry — the row replicates to nodes that
    /// may not hold the account.
    pub requested_by: String,
    /// Display name joined by the answering node, so an inbox need not join the
    /// directory. Falls back to the id when the account is gone.
    pub requested_by_label: String,
    pub requested_at: String,
    pub expires_at: String,
    pub state: String,
    /// Who decided, and how — empty while `state` is 'pending' or 'expired'.
    /// A decision that LOST (an approval outranked by a rejection) stays in the
    /// store as its own row and is what the audit reads; these three fields
    /// carry the decision that won.
    pub decided_by: String,
    pub decided_at: String,
    pub decision_note: String,
}

// =============================================================================
// Settings
// =============================================================================

/// `vm_instance_settings` (§4.1) as one document — the environment settings
/// screen reads and writes the whole set, so a partial update cannot lose a
/// key it never rendered. `visibility` is 'all' (hosts without a grant stay
/// visible) | 'granted' (they are hidden). `default_size_preset` is 's' | 'm'
/// | 'l' (§17.3), `default_firmware` is 'uefi' | 'bios', `ssh_key_source` is
/// 'profile' | 'paste' | 'none', `autostart_policy` is 'off' | 'ordered',
/// `ha_fencing` is 'none' | 'watchdog' | 'power'.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct VmInstanceSettings {
    pub visibility: String,
    pub default_pool_id: Option<String>,
    pub default_network_id: Option<String>,
    pub default_image_id: Option<String>,
    pub default_size_preset: String,
    pub default_firmware: String,
    pub ssh_key_source: String,
    /// The baseline CPU model definition every new machine is pinned to, so a
    /// machine stays migratable across a mixed fleet (§5.4).
    pub cpu_baseline_xml: String,
    pub machine_type: String,
    pub autostart_policy: String,
    pub ha_enabled: bool,
    pub ha_coordinator_node_id: Option<String>,
    pub ha_fencing: String,
    pub overcommit_ratio: f64,
}

// =============================================================================
// Dashboard summary
// =============================================================================

/// One item of the P01 inbox ("Czekają na Ciebie"). `kind` is 'admin_consent'
/// | 'host_restart' | 'service_restart' | 'credential_expired' |
/// 'host_unreachable' | 'job_failed' | 'access_request' (§17.4) and is also
/// the i18n stem: a reader renders `inbox.<kind>.title`, `inbox.<kind>.detail`
/// and `inbox.<kind>.cta`, filling all three from the same `params`.
///
/// There is no inbox TABLE and the item carries no ready sentence: every kind
/// but `access_request` is derived on demand — `admin_consent` from
/// `vm_host_settings`, `host_restart` / `service_restart` from the pending
/// saga step, `credential_expired` from `vm_connectors.last_probe_at`,
/// `host_unreachable` from `vm_hosts.status`, `job_failed` from `vm_jobs`.
/// `access_request` is the one kind with no source yet: filing one is an
/// authorization design that belongs with `vm_host_grants`, so P00's "Poproś
/// administratora" and the row behind it are specified in step 6 (host
/// grants), not here. Until then no node emits an item of that kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VmInboxItem {
    /// Stable across recomputations of the SAME item, because "Później"
    /// (`InboxSnoozeRequest`) is keyed by it and nothing else: no inbox table
    /// exists, so every item is derived on demand and a producer that made
    /// `item_id` from a timestamp or a row counter would hand the snooze a key
    /// that never comes back. The shape to use is `<kind>:<host_id|job_id>` —
    /// the identity of the thing the item is ABOUT.
    pub item_id: String,
    pub kind: String,
    /// Substitutions for the three strings named by `kind`: 'host', 'guest',
    /// 'user', 'count', 'job' — whichever that kind's translations use.
    pub params: Vec<VmTextParam>,
    pub host_id: Option<String>,
    pub host_name: String,
    pub job_id: Option<String>,
    pub requested_by: String,
    pub requested_at: String,
    /// A dashboard route fragment the tile navigates to, never a URL and never
    /// a label — the CTA's text comes from `kind`.
    pub cta_route: String,
    /// A high-risk item opened on a node that is not operator-flagged: the
    /// mobile inbox shows it read-only with `read_only_reason` instead of a
    /// button that would fail closed on the executor (§7.1).
    pub read_only: bool,
    pub read_only_reason: VmText,
}

/// The P01 tiles plus everything the first-run variant (P00) needs to decide
/// which empty state to draw. `local_host_status` and the two `local_*` fields
/// below it are the probe verdict of the node the browser is talking to —
/// "gotowy" / "brakuje: qemu, libvirt, swtpm" / "niewspierany: brak VT-x".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct VmSummary {
    pub guests_total: u32,
    pub guests_running: u32,
    pub hosts_total: u32,
    pub hosts_ready: u32,
    pub hosts_needs_install: u32,
    pub hosts_unreachable: u32,
    pub jobs_running: u32,
    pub jobs_failed: u32,
    pub inbox: Vec<VmInboxItem>,
    /// The caller holds `vm.create`; without it P00 offers "Poproś
    /// administratora". What that button WRITES is designed in step 6 together
    /// with `vm_host_grants` — a request that an admin on another node decides
    /// needs the ownership and convergence rules of the registry, which do not
    /// exist yet — so phase 0 carries the flag and not the write.
    pub can_create_guest: bool,
    pub local_host_id: Option<String>,
    pub local_host_status: String,
    /// `FeatureState.id`s of the environment features this node still needs —
    /// the ids listed at `VmHostEnvironment.features`, not package names: what
    /// a feature is called in a package manager is one distribution's answer
    /// and this field is read by a browser that has an i18n entry per id.
    /// Empty means either "nothing missing" or "not probed yet", and
    /// `local_host_status` is what tells those apart.
    pub local_missing_features: Vec<String>,
    pub local_unsupported_reason: VmText,
    /// How many inbox items exist for this caller. The node caps `inbox`
    /// itself, so the tile can say "3 z 47" instead of the browser believing a
    /// truncated list is the whole of it.
    pub inbox_total: u32,
    /// **The caller's most recent access request in this environment, by
    /// `requested_at`, in whatever state it is.**
    ///
    /// That one sentence is the whole definition, and "most recent" rather than
    /// "open" is the deliberate half of it: P00 has to draw the refusal
    /// ("Administrator odmówił: …") and the expiry as well as the wait, and a
    /// field defined as "the open one" would go empty exactly when the user
    /// most needs to be told what happened. Every click files a NEW row, so
    /// "most recent" always names one row and never needs a second concept to
    /// say which.
    #[serde(default)]
    pub access_request: Option<VmAccessRequest>,
}

// =============================================================================
// Payload
// =============================================================================

/// Every TentaVM request/response. Ciborium tags variants by NAME, but the
/// order is still the contract — append-only, never insert or reorder — and no
/// variant or field may be renamed without updating the frontend and the pin
/// tests below (`tentavm_wire_enums_are_pinned`,
/// `tentavm_wire_struct_fields_are_pinned`, `tentavm_wire_golden`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TentaVmPayload {
    // ----- dashboard (P01, and its first-run variant P00) -----
    SummaryRequest {
        instance_id: String,
    },
    SummaryResponse {
        summary: VmSummary,
    },

    // ----- hosts (H01, H02) -----
    /// Every mesh node plus every connector host of the environment, filtered
    /// by the caller's `view` grants when `visibility` is 'granted'.
    HostsListRequest {
        instance_id: String,
    },
    HostsListResponse {
        hosts: Vec<VmHost>,
        local_host_id: Option<String>,
        /// The environment's `visibility` setting, so the list can explain why
        /// a host the user knows exists is not in it.
        visibility: String,
    },
    HostGetRequest {
        instance_id: String,
        host_id: String,
    },
    /// `environment` is the last probe result of that host; None means the host
    /// has never been probed (or answered), which is H02's "Sonduj" state.
    HostGetResponse {
        host: VmHost,
        environment: Option<VmHostEnvironment>,
    },
    /// Runs the environment probe on the host that owns the hardware. Without
    /// `refresh` the owner may answer from `vm_probe_cache`.
    HostProbeRequest {
        instance_id: String,
        host_id: String,
        #[serde(default)]
        refresh: bool,
    },
    HostProbeResponse {
        host_id: String,
        environment: VmHostEnvironment,
    },

    // ----- jobs (Z01) -----
    /// `host_id` narrows the list to one host's jobs, `states` to a subset of
    /// job states; an empty `states` means every state.
    JobsListRequest {
        instance_id: String,
        #[serde(default)]
        host_id: Option<String>,
        #[serde(default)]
        states: Vec<String>,
        #[serde(default)]
        limit: u32,
    },
    JobsListResponse {
        jobs: Vec<VmJob>,
    },
    /// Forwarded to the job's owner node — `vm_job_logs` never leaves it.
    JobGetRequest {
        instance_id: String,
        job_id: String,
    },
    JobGetResponse {
        job: VmJob,
        log: Vec<VmJobLogLine>,
    },

    // ----- host grants (H06) -----
    HostGrantsListRequest {
        instance_id: String,
        host_id: String,
    },
    HostGrantsListResponse {
        host_id: String,
        grants: Vec<VmHostGrant>,
        candidates: Vec<VmGrantCandidate>,
        /// The caller may write this matrix (`manage` on the host, or
        /// `vm.admin`); without it H06 renders read-only.
        can_edit: bool,
    },
    /// The complete desired grant set of one host — rows absent from `grants`
    /// are removed. Answers with `HostGrantsListResponse`, so the matrix
    /// redraws from what was actually stored.
    HostGrantsSetRequest {
        instance_id: String,
        host_id: String,
        grants: Vec<VmHostGrantInput>,
    },

    // ----- environment settings -----
    SettingsGetRequest {
        instance_id: String,
    },
    SettingsGetResponse {
        settings: VmInstanceSettings,
        can_edit: bool,
    },
    /// The whole settings document. Answers with `SettingsGetResponse`.
    SettingsSetRequest {
        instance_id: String,
        settings: VmInstanceSettings,
    },

    // ----- appended after the first review; the enum is append-only -----
    /// Stops a running job. What that means for THIS job is its own
    /// `cancel_semantics` ('cooperative' stops at the next step boundary,
    /// 'compensating' rolls the finished steps back, 'none' is refused), so
    /// the UI can state the consequence before it asks. Answers with
    /// `JobGetResponse`, which carries the state the cancel actually reached.
    JobCancelRequest {
        instance_id: String,
        job_id: String,
    },
    /// "Później" on one inbox item: hide it from this user's inbox for
    /// `snooze_secs`. The window is the caller's decision (P01 sends 24 h) —
    /// there is no stored default to fall back to, so the field is required
    /// rather than treating zero as a mode. Answers with `SummaryResponse`,
    /// which is the honest confirmation here: this write changes the CALLER's
    /// own inbox, and the answer is that inbox with the item gone.
    InboxSnoozeRequest {
        instance_id: String,
        item_id: String,
        snooze_secs: u32,
    },

    // ----- access requests (P00), appended at END -----
    /// "Poproś administratora" — files one request. `scope` and the pair
    /// (`host_id`, `role`) follow `VmAccessRequest`; the node supplies
    /// `requested_at` and `expires_at` from its own clock and policy, because a
    /// requester choosing their own term would choose forever.
    ///
    /// Answers with `SummaryResponse`, which is the honest confirmation: the
    /// write changes what P00 shows, and the answer is that screen with the
    /// request on it.
    AccessRequestFileRequest {
        instance_id: String,
        scope: String,
        #[serde(default)]
        host_id: Option<String>,
        #[serde(default)]
        role: String,
        reason: String,
    },
    /// An administrator's decision on one request. `decision` is 'approve' |
    /// 'reject'.
    ///
    /// `content_digest` binds the decision to the row that was SHOWN (§15:
    /// "wiążą zgodę z hashem planu pokazanego użytkownikowi"). The stored row
    /// is immutable, so the requester cannot swap the text under the reader;
    /// what this catches is the other case — an administrator deciding from a
    /// list their node had not yet caught up on, where the row they read and
    /// the row they are deciding about differ by replication rather than by
    /// anybody's edit.
    ///
    /// Answers with `SummaryResponse`.
    AccessRequestDecideRequest {
        instance_id: String,
        request_id: String,
        decision: String,
        #[serde(default)]
        note: String,
        content_digest: String,
    },
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_body::MessageBody;
    use crate::wire_pin::{self, hex_bytes, name_digest};

    /// This very file, read at compile time. The byte goldens pin a few
    /// SHAPES; the declaration goldens pin the top-level `pub struct` and
    /// `pub enum` declarations of THIS file — names, order, field types and
    /// the serde attributes that rewrite either — and nothing beyond it.
    /// `crate::wire_pin` documents what that does and does not reach, and
    /// which of its assumptions `assert_parseable` proves rather than asserts.
    const SOURCE: &str = include_str!("tentavm.rs");

    /// The parser's five assumptions, checked against this module rather than
    /// stated about it — see `wire_pin::assert_parseable` for the list. The
    /// fifth is the recount of enum variants: a shape the collector drops
    /// fails here instead of quietly shrinking a digest.
    #[test]
    fn tentavm_source_is_parseable() {
        wire_pin::assert_parseable(SOURCE);
    }

    /// Every `pub enum` of the module. `TentaVmPayload` is the only one today;
    /// reading the whole SET rather than that one name is deliberate, so the
    /// next domain enum is covered the day it is declared and not the day
    /// someone remembers — a domain enum is a wire contract exactly like the
    /// payload one, since ciborium tags its variants by name and encodes their
    /// fields as a map.
    #[test]
    fn tentavm_wire_enums_are_pinned() {
        let enums = wire_pin::wire_enums(SOURCE);
        let names: Vec<String> = enums.iter().map(|item| item.name.clone()).collect();
        assert_eq!(
            names,
            vec!["TentaVmPayload".to_string()],
            "wire enum SET or its order changed. A new enum needs its own row in the table below before this test can pass."
        );

        // (enum, member count, digest of its attributes + variants)
        let pinned: &[(&str, usize, u64)] = &[("TentaVmPayload", 22, 0xa515_eb9e_3ed7_0d9e)];
        assert_eq!(pinned.len(), enums.len());
        for (name, count, digest) in pinned {
            let item = enums
                .iter()
                .find(|item| &item.name == name)
                .unwrap_or_else(|| panic!("enum '{name}' is gone from the wire module"));
            let entries = item.entries();
            assert_eq!(
                item.members.len(),
                *count,
                "'{name}' variant COUNT changed. Appending is fine — update the count and the digest here in the same commit. Live entries:\n{}",
                entries.join("\n")
            );
            assert_eq!(
                name_digest(&entries),
                *digest,
                "'{name}' variant NAMES, their FIELDS, their ORDER or a serde attribute changed. ciborium tags variants by name and encodes their fields as a map, so any of those moves the wire while every round-trip test stays green. Live entries:\n{}",
                entries.join("\n")
            );
        }
    }

    /// The payload STRUCTS, pinned the same way — the half the byte goldens
    /// never touch. `VmHost` alone carries 28 fields the dashboard reads by
    /// name. Each field enters the digest as `name: Type` together with its
    /// serde attributes, and the STRUCT's own attributes lead the list, so
    /// `#[serde(rename_all = "camelCase")]` — which renames every key at once
    /// from above the declaration — fails here just like a single renamed
    /// field.
    #[test]
    fn tentavm_wire_struct_fields_are_pinned() {
        let structs = wire_pin::wire_structs(SOURCE);
        let names: Vec<String> = structs.iter().map(|item| item.name.clone()).collect();
        assert_eq!(
            names.len(),
            17,
            "wire struct COUNT changed. Live structs:\n{}",
            names.join("\n")
        );
        assert_eq!(
            name_digest(&names),
            0x3460_42ed_ad16_2d06,
            "wire struct NAMES or their DECLARATION ORDER changed. Reordering two struct \
             blocks does not move the wire — only field and variant order does — so \
             that case is a safe digest update, not a break. Live structs:\n{}",
            names.join("\n")
        );

        // (struct, field count, digest of its attributes + "name: Type" fields)
        let pinned: &[(&str, usize, u64)] = &[
            ("VmTextParam", 2, 0x5c20_9aa7_1fd5_d433),
            ("VmText", 2, 0xf200_d1f3_a70a_d6b4),
            ("VmEngine", 7, 0xcff5_39ba_c2d3_20d6),
            ("VmCapability", 3, 0xed00_53e0_6901_5c2f),
            ("VmHost", 28, 0x123c_5345_81bb_e059),
            ("VmVirtSupport", 9, 0x1b62_dea6_f140_9588),
            ("VmHostEnvironment", 21, 0x0712_8741_aaf8_9963),
            ("VmJobStep", 7, 0xf243_f475_0198_5945),
            ("VmJobLogLine", 4, 0xa2c5_cdfb_d07f_5de9),
            ("VmJob", 21, 0x6536_9a1c_a1fe_ea74),
            ("VmHostGrant", 7, 0x240c_5102_d49e_2b20),
            ("VmHostGrantInput", 3, 0x0ce6_9503_50ec_8411),
            ("VmGrantCandidate", 3, 0xd6b5_d1ff_ba8e_516c),
            ("VmAccessRequest", 13, 0xb558_4ed3_adcf_7b87),
            ("VmInstanceSettings", 14, 0xac0c_23e8_92e1_d17b),
            ("VmInboxItem", 11, 0xdadc_ce02_032a_f1d3),
            ("VmSummary", 16, 0x2216_6e05_e9f9_fa19),
        ];
        assert_eq!(pinned.len(), structs.len());
        for (name, count, digest) in pinned {
            let item = structs
                .iter()
                .find(|item| &item.name == name)
                .unwrap_or_else(|| panic!("struct '{name}' is gone from the wire module"));
            let entries = item.entries();
            assert_eq!(
                item.members.len(),
                *count,
                "'{name}' field COUNT changed. Adding a field with #[serde(default)] is the supported move — update the count and digest here. Live entries:\n{}",
                entries.join("\n")
            );
            assert_eq!(
                name_digest(&entries),
                *digest,
                "'{name}' field NAMES, TYPES, ORDER or a serde attribute on the struct or one of its fields changed. The dashboard decodes these by name and the wire encodes them by type, and a round-trip test cannot see any of it because it re-encodes with the new declaration. Live entries:\n{}",
                entries.join("\n")
            );
        }
    }

    /// Golden wire snapshot: ciborium encodes enum variants as a 1-element map
    /// keyed by the variant NAME (external tagging). Pinning exact bytes turns
    /// any accidental rename of a variant, a field or the
    /// `MessageBody::TentaVmBody` tag into a test failure.
    #[test]
    fn tentavm_wire_golden() {
        let req = TentaVmPayload::HostsListRequest {
            instance_id: "vm1".to_string(),
        };
        let bytes = crate::cbor::encode(&req).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes("a170486f7374734c69737452657175657374a16b696e7374616e63655f696463766d31"),
            "HostsListRequest wire drift"
        );

        let body = MessageBody::TentaVmBody(req);
        let bytes = crate::cbor::encode(&body).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a16b54656e7461566d426f6479a170486f7374734c69737452657175657374a16b696e7374616e6\
                 3655f696463766d31"
            ),
            "MessageBody::TentaVmBody wire drift"
        );

        let probe = TentaVmPayload::HostProbeRequest {
            instance_id: "vm1".to_string(),
            host_id: "h1".to_string(),
            refresh: true,
        };
        assert_eq!(
            crate::cbor::encode(&probe).expect("encode"),
            hex_bytes(
                "a170486f737450726f626552657175657374a36b696e7374616e63655f696463766d3167686f73\
                 745f69646268316772656672657368f5"
            ),
            "HostProbeRequest wire drift"
        );
    }

    /// The three request fields the dashboard is allowed to omit must decode
    /// from the encoder's minimal JSON — that is what `#[serde(default)]`
    /// buys, and the wasm codec builds every request from exactly such an
    /// object.
    #[test]
    fn the_optional_request_fields_decode_from_minimal_json() {
        let json = serde_json::json!({
            "HostProbeRequest": { "instance_id": "vm1", "host_id": "h1" }
        });
        let decoded: TentaVmPayload = serde_json::from_value(json).expect("decode");
        assert_eq!(
            decoded,
            TentaVmPayload::HostProbeRequest {
                instance_id: "vm1".to_string(),
                host_id: "h1".to_string(),
                refresh: false,
            }
        );

        let json = serde_json::json!({ "JobsListRequest": { "instance_id": "vm1" } });
        let decoded: TentaVmPayload = serde_json::from_value(json).expect("decode");
        assert_eq!(
            decoded,
            TentaVmPayload::JobsListRequest {
                instance_id: "vm1".to_string(),
                host_id: None,
                states: Vec::new(),
                limit: 0,
            }
        );

        // Every other request has only required fields, so an empty object
        // must NOT decode — a dashboard that forgets `instance_id` has to fail
        // at the codec, not silently address the wrong environment. The same
        // holds for a snooze without a window: there is no stored default for
        // it to mean.
        let json = serde_json::json!({ "SummaryRequest": {} });
        assert!(serde_json::from_value::<TentaVmPayload>(json).is_err());
        let json = serde_json::json!({ "JobCancelRequest": { "instance_id": "vm1" } });
        assert!(serde_json::from_value::<TentaVmPayload>(json).is_err());
        let json = serde_json::json!({
            "InboxSnoozeRequest": { "instance_id": "vm1", "item_id": "in-1" }
        });
        assert!(serde_json::from_value::<TentaVmPayload>(json).is_err());
    }

    /// The two write requests appended after the first review, through the
    /// same CBOR the browser encodes. Each answers with a response that
    /// already round-trips above, so only the request side is new here.
    #[test]
    fn the_appended_write_requests_round_trip() {
        for body in [
            MessageBody::TentaVmBody(TentaVmPayload::JobCancelRequest {
                instance_id: "vm1".to_string(),
                job_id: "j-1".to_string(),
            }),
            MessageBody::TentaVmBody(TentaVmPayload::InboxSnoozeRequest {
                instance_id: "vm1".to_string(),
                item_id: "in-1".to_string(),
                snooze_secs: 86_400,
            }),
        ] {
            let back: MessageBody =
                crate::cbor::decode(&crate::cbor::encode(&body).expect("encode")).expect("decode");
            assert_eq!(back, body);
        }
    }

    /// The answers the dashboard reads back travel through the same CBOR the
    /// browser decodes; every field of every struct must survive the round
    /// trip inside a `MessageBody`.
    #[test]
    fn the_phase_zero_answers_round_trip() {
        let engine = VmEngine {
            id: "kvm".to_string(),
            status: "needs_consent".to_string(),
            version: Some("10.0.0".to_string()),
            kinds: vec!["vm".to_string()],
            detail: "libvirt 10.0.0, QEMU 8.2.2".to_string(),
            consent_required: true,
            consent_granted: false,
        };
        let capability = VmCapability {
            id: "snapshot_revert".to_string(),
            supported: false,
            reason: VmText {
                key: "capability.needs_libvirt".to_string(),
                params: vec![
                    VmTextParam {
                        name: "have".to_string(),
                        value: "10.0.0".to_string(),
                    },
                    VmTextParam {
                        name: "need".to_string(),
                        value: "11.1.0".to_string(),
                    },
                ],
            },
        };
        let host = VmHost {
            host_id: "h-dev-ryzen".to_string(),
            kind: "node".to_string(),
            node_id: Some("n-dev-ryzen".to_string()),
            connector_id: None,
            external_ref: None,
            display_name: "dev-ryzen".to_string(),
            status: "needs_install".to_string(),
            status_reason: VmText {
                key: "host.needs_install".to_string(),
                params: vec![VmTextParam {
                    name: "count".to_string(),
                    value: "3".to_string(),
                }],
            },
            online: true,
            is_local: true,
            owner_node_id: "n-dev-ryzen".to_string(),
            owner_epoch: 7,
            os_name: "Ubuntu".to_string(),
            os_version: "24.04".to_string(),
            arch: "x86_64".to_string(),
            cpu_cores: 16,
            cpu_used_pct: 12.5,
            ram_bytes: 137_438_953_472,
            ram_used_bytes: 24_696_061_952,
            storage_bytes: 2_000_398_934_016,
            storage_used_bytes: 412_316_860_416,
            guests_total: 0,
            guests_running: 0,
            engines: vec![engine.clone()],
            capabilities: vec![capability.clone()],
            your_role: "manage".to_string(),
            last_seen_at: Some("2026-09-03T10:00:00Z".to_string()),
            updated_at: Some("2026-09-03T10:00:01Z".to_string()),
        };
        let environment = VmHostEnvironment {
            platform: "linux".to_string(),
            full_support: true,
            os_name: "Ubuntu".to_string(),
            os_version: "24.04".to_string(),
            kernel: "6.8.0-45-generic".to_string(),
            hostname: "dev-ryzen".to_string(),
            arch: "x86_64".to_string(),
            package_manager: "apt".to_string(),
            virt: VmVirtSupport {
                hardware_virtualization: true,
                cpu_flag: "svm".to_string(),
                kvm_device: true,
                nested: true,
                iommu: true,
                iommu_groups: 21,
                rebar: true,
                sysfb: true,
                detail: VmText {
                    key: "virt.ok".to_string(),
                    params: vec![VmTextParam {
                        name: "flag".to_string(),
                        value: "svm".to_string(),
                    }],
                },
            },
            libvirt_version: Some("10.0.0".to_string()),
            libvirt_daemon_mode: "monolithic".to_string(),
            qemu_version: Some("8.2.2".to_string()),
            security_module: "apparmor".to_string(),
            tentavm_account: false,
            watchdog_device: true,
            features: vec![FeatureState {
                id: "kvm_base".to_string(),
                status: "missing".to_string(),
                version: None,
                required_version: Some("8.0.0".to_string()),
                binaries: vec!["qemu-system-x86_64".to_string()],
                kernel_module: Some("kvm_amd".to_string()),
                packages: vec!["qemu-system-x86".to_string(), "libvirt-clients".to_string()],
                detail: "missing qemu-system-x86_64".to_string(),
                optional: false,
            }],
            engines: vec![engine],
            capabilities: vec![capability],
            missing_packages: vec!["qemu-system-x86".to_string(), "swtpm".to_string()],
            requires_service_restart: true,
            probed_at: "2026-09-03T10:00:01Z".to_string(),
        };
        let body = MessageBody::TentaVmBody(TentaVmPayload::HostGetResponse {
            host: host.clone(),
            environment: Some(environment.clone()),
        });
        let back: MessageBody =
            crate::cbor::decode(&crate::cbor::encode(&body).expect("encode")).expect("decode");
        assert_eq!(back, body);

        let body = MessageBody::TentaVmBody(TentaVmPayload::HostsListResponse {
            hosts: vec![host],
            local_host_id: Some("h-dev-ryzen".to_string()),
            visibility: "granted".to_string(),
        });
        let back: MessageBody =
            crate::cbor::decode(&crate::cbor::encode(&body).expect("encode")).expect("decode");
        assert_eq!(back, body);

        let body = MessageBody::TentaVmBody(TentaVmPayload::HostProbeResponse {
            host_id: "h-dev-ryzen".to_string(),
            environment,
        });
        let back: MessageBody =
            crate::cbor::decode(&crate::cbor::encode(&body).expect("encode")).expect("decode");
        assert_eq!(back, body);

        let body = MessageBody::TentaVmBody(TentaVmPayload::SummaryResponse {
            summary: VmSummary {
                guests_total: 12,
                guests_running: 9,
                hosts_total: 3,
                hosts_ready: 1,
                hosts_needs_install: 1,
                hosts_unreachable: 1,
                jobs_running: 2,
                jobs_failed: 1,
                inbox: vec![VmInboxItem {
                    item_id: "in-1".to_string(),
                    kind: "admin_consent".to_string(),
                    params: vec![
                        VmTextParam {
                            name: "host".to_string(),
                            value: "dev-ryzen".to_string(),
                        },
                        VmTextParam {
                            name: "engine".to_string(),
                            value: "kvm".to_string(),
                        },
                    ],
                    host_id: Some("h-dev-ryzen".to_string()),
                    host_name: "dev-ryzen".to_string(),
                    job_id: None,
                    requested_by: "u-anna".to_string(),
                    requested_at: "2026-09-03T09:30:00Z".to_string(),
                    cta_route: "#/tentavm?instance=vm1&host=h-dev-ryzen".to_string(),
                    read_only: true,
                    read_only_reason: VmText {
                        key: "inbox.operator_node_required".to_string(),
                        params: Vec::new(),
                    },
                }],
                can_create_guest: true,
                local_host_id: Some("h-dev-ryzen".to_string()),
                local_host_status: "needs_install".to_string(),
                local_missing_features: vec!["kvm_base".to_string()],
                local_unsupported_reason: VmText::default(),
                inbox_total: 4,
                // The P00 half of this answer: a request the caller filed and
                // an administrator has not answered yet. `expires_at` and
                // `state` both travel, which is what lets the screen say
                // "wygasa za" and stop offering a decision the server refuses.
                access_request: Some(VmAccessRequest {
                    request_id: "ar-1".to_string(),
                    scope: "host_role".to_string(),
                    host_id: Some("h-dev-ryzen".to_string()),
                    role: "deploy".to_string(),
                    reason: "potrzebuję na test migracji".to_string(),
                    requested_by: "u-1".to_string(),
                    requested_by_label: "Ala Kowalska".to_string(),
                    requested_at: "2026-09-06T10:00:00Z".to_string(),
                    expires_at: "2026-09-13T10:00:00Z".to_string(),
                    state: "pending".to_string(),
                    decided_by: String::new(),
                    decided_at: String::new(),
                    decision_note: String::new(),
                }),
            },
        });
        let back: MessageBody =
            crate::cbor::decode(&crate::cbor::encode(&body).expect("encode")).expect("decode");
        assert_eq!(back, body);
    }

    /// Jobs, grants and settings — the three write-capable families of phase 0.
    /// The grant and settings writes answer with the READ response, so both
    /// directions are pinned here.
    #[test]
    fn the_job_grant_and_settings_answers_round_trip() {
        let body = MessageBody::TentaVmBody(TentaVmPayload::JobGetResponse {
            job: VmJob {
                job_id: "j-1".to_string(),
                instance_id: "vm1".to_string(),
                kind: "host_environment_install".to_string(),
                label: VmText {
                    key: "job.host_environment_install".to_string(),
                    params: vec![
                        VmTextParam {
                            name: "host".to_string(),
                            value: "dev-ryzen".to_string(),
                        },
                        VmTextParam {
                            name: "count".to_string(),
                            value: "6".to_string(),
                        },
                    ],
                },
                guest_id: None,
                guest_name: String::new(),
                source_host_id: None,
                target_host_id: Some("h-dev-ryzen".to_string()),
                host_name: "dev-ryzen".to_string(),
                owner_node_id: "n-dev-ryzen".to_string(),
                state: "waiting_restart".to_string(),
                progress_pct: 60,
                phase: VmText {
                    key: "job.phase.service_restart".to_string(),
                    params: Vec::new(),
                },
                steps: vec![VmJobStep {
                    id: "packages".to_string(),
                    label: VmText {
                        key: "job.step.packages".to_string(),
                        params: Vec::new(),
                    },
                    state: "done".to_string(),
                    progress_pct: 100,
                    detail: VmText {
                        key: "job.step.packages.detail".to_string(),
                        params: vec![VmTextParam {
                            name: "count".to_string(),
                            value: "6".to_string(),
                        }],
                    },
                    started_at: Some("2026-09-03T10:01:00Z".to_string()),
                    finished_at: Some("2026-09-03T10:04:00Z".to_string()),
                }],
                cancel_semantics: "compensating".to_string(),
                resume_after_restart: true,
                error: String::new(),
                created_by: "u-anna".to_string(),
                created_at: "2026-09-03T10:00:59Z".to_string(),
                started_at: Some("2026-09-03T10:01:00Z".to_string()),
                finished_at: None,
            },
            log: vec![VmJobLogLine {
                at: "2026-09-03T10:01:02Z".to_string(),
                level: "info".to_string(),
                step_id: "packages".to_string(),
                text: "apt-get install qemu-system-x86".to_string(),
            }],
        });
        let back: MessageBody =
            crate::cbor::decode(&crate::cbor::encode(&body).expect("encode")).expect("decode");
        assert_eq!(back, body);

        let body = MessageBody::TentaVmBody(TentaVmPayload::HostGrantsListResponse {
            host_id: "h-dev-ryzen".to_string(),
            grants: vec![VmHostGrant {
                host_id: "h-dev-ryzen".to_string(),
                subject_kind: "group".to_string(),
                subject_id: "g-devops".to_string(),
                subject_label: "DevOps".to_string(),
                role: "deploy".to_string(),
                granted_by: "u-anna".to_string(),
                granted_at: "2026-09-03T08:00:00Z".to_string(),
            }],
            candidates: vec![VmGrantCandidate {
                subject_kind: "user".to_string(),
                subject_id: "u-bartek".to_string(),
                subject_label: "Bartek".to_string(),
            }],
            can_edit: true,
        });
        let back: MessageBody =
            crate::cbor::decode(&crate::cbor::encode(&body).expect("encode")).expect("decode");
        assert_eq!(back, body);

        let settings = VmInstanceSettings {
            visibility: "granted".to_string(),
            default_pool_id: Some("p-local".to_string()),
            default_network_id: Some("net-default".to_string()),
            default_image_id: None,
            default_size_preset: "m".to_string(),
            default_firmware: "uefi".to_string(),
            ssh_key_source: "profile".to_string(),
            cpu_baseline_xml: "<cpu mode='custom'/>".to_string(),
            machine_type: "pc-q35-9.2".to_string(),
            autostart_policy: "ordered".to_string(),
            ha_enabled: false,
            ha_coordinator_node_id: None,
            ha_fencing: "watchdog".to_string(),
            overcommit_ratio: 1.5,
        };
        let write = MessageBody::TentaVmBody(TentaVmPayload::SettingsSetRequest {
            instance_id: "vm1".to_string(),
            settings: settings.clone(),
        });
        let back: MessageBody =
            crate::cbor::decode(&crate::cbor::encode(&write).expect("encode")).expect("decode");
        assert_eq!(back, write);

        let read = MessageBody::TentaVmBody(TentaVmPayload::SettingsGetResponse {
            settings,
            can_edit: true,
        });
        let back: MessageBody =
            crate::cbor::decode(&crate::cbor::encode(&read).expect("encode")).expect("decode");
        assert_eq!(back, read);
    }
}
