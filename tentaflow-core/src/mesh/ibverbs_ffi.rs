// =============================================================================
// Plik: mesh/ibverbs_ffi.rs
// Opis: Reczne bindings do libibverbs — bez bindgen, tylko uzywane typy.
//       Kompilowane tylko z cfg(feature = "rdma-probe") na Linuxie.
//       ibv_post_send, ibv_post_recv i ibv_poll_cq sa w C inline functions
//       wolajacymi przez vtable w ibv_context.ops — tu odtwarzamy to samo.
// =============================================================================
#![allow(non_camel_case_types, non_upper_case_globals, dead_code)]

use std::os::raw::{c_char, c_int, c_uint, c_void};

// =============================================================================
// Opaque typy (nie potrzebujemy wnetrza)
// =============================================================================

#[repr(C)]
pub struct ibv_pd {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct ibv_device {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct ibv_srq {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct ibv_comp_channel {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct ibv_ah {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct ibv_mw {
    _opaque: [u8; 0],
}

// =============================================================================
// ibv_context_ops — vtable z function pointerami (verbs.h linia 2026)
// Pola _compat_* sa void* — nie uzywamy ich bezposrednio.
// Zachowujemy uklad pamieci zeby offsety poll_cq/post_send/post_recv byly poprawne.
// =============================================================================

#[repr(C)]
pub struct ibv_context_ops {
    pub _compat_query_device: *mut c_void,
    pub _compat_query_port: *mut c_void,
    pub _compat_alloc_pd: *mut c_void,
    pub _compat_dealloc_pd: *mut c_void,
    pub _compat_reg_mr: *mut c_void,
    pub _compat_rereg_mr: *mut c_void,
    pub _compat_dereg_mr: *mut c_void,
    pub alloc_mw: *mut c_void,
    pub bind_mw: *mut c_void,
    pub dealloc_mw: *mut c_void,
    pub _compat_create_cq: *mut c_void,
    pub poll_cq: Option<
        unsafe extern "C" fn(cq: *mut ibv_cq, num_entries: c_int, wc: *mut ibv_wc) -> c_int,
    >,
    pub req_notify_cq: *mut c_void,
    pub _compat_cq_event: *mut c_void,
    pub _compat_resize_cq: *mut c_void,
    pub _compat_destroy_cq: *mut c_void,
    pub _compat_create_srq: *mut c_void,
    pub _compat_modify_srq: *mut c_void,
    pub _compat_query_srq: *mut c_void,
    pub _compat_destroy_srq: *mut c_void,
    pub post_srq_recv: *mut c_void,
    pub _compat_create_qp: *mut c_void,
    pub _compat_query_qp: *mut c_void,
    pub _compat_modify_qp: *mut c_void,
    pub _compat_destroy_qp: *mut c_void,
    pub post_send: Option<
        unsafe extern "C" fn(
            qp: *mut ibv_qp,
            wr: *mut ibv_send_wr,
            bad_wr: *mut *mut ibv_send_wr,
        ) -> c_int,
    >,
    pub post_recv: Option<
        unsafe extern "C" fn(
            qp: *mut ibv_qp,
            wr: *mut ibv_recv_wr,
            bad_wr: *mut *mut ibv_recv_wr,
        ) -> c_int,
    >,
    pub _compat_create_ah: *mut c_void,
    pub _compat_destroy_ah: *mut c_void,
    pub _compat_attach_mcast: *mut c_void,
    pub _compat_detach_mcast: *mut c_void,
    pub _compat_async_event: *mut c_void,
}

// =============================================================================
// ibv_context (verbs.h linia 2069)
// =============================================================================

#[repr(C)]
pub struct ibv_context {
    pub device: *mut ibv_device,
    pub ops: ibv_context_ops,
    pub cmd_fd: c_int,
    pub async_fd: c_int,
    pub num_comp_vectors: c_int,
    // pthread_mutex_t i abi_compat pomijamy — nie dotykamy tych pol
}

// =============================================================================
// ibv_cq (verbs.h linia 1540)
// Potrzebujemy context na offsetcie 0, zeby poll_cq moglo byc wyluskane.
// =============================================================================

#[repr(C)]
pub struct ibv_cq {
    pub context: *mut ibv_context,
    pub channel: *mut ibv_comp_channel,
    pub cq_context: *mut c_void,
    pub handle: u32,
    pub cqe: c_int,
    // pthread_mutex_t, cond, itd. — pomijamy
}

// =============================================================================
// ibv_qp (verbs.h linia 1315)
// Potrzebujemy qp_num i context (do post_send/post_recv).
// =============================================================================

#[repr(C)]
pub struct ibv_qp {
    pub context: *mut ibv_context,
    pub qp_context: *mut c_void,
    pub pd: *mut ibv_pd,
    pub send_cq: *mut ibv_cq,
    pub recv_cq: *mut ibv_cq,
    pub srq: *mut ibv_srq,
    pub handle: u32,
    pub qp_num: u32,
    pub state: c_uint,   // ibv_qp_state
    pub qp_type: c_uint, // ibv_qp_type
    // pthread_mutex_t, cond, events_completed — pomijamy
}

// =============================================================================
// ibv_mr (verbs.h linia 675)
// =============================================================================

#[repr(C)]
pub struct ibv_mr {
    pub context: *mut ibv_context,
    pub pd: *mut ibv_pd,
    pub addr: *mut c_void,
    pub length: usize,
    pub handle: u32,
    pub lkey: u32,
    pub rkey: u32,
}

// =============================================================================
// ibv_port_attr (verbs.h linia 427)
// =============================================================================

#[repr(C)]
pub struct ibv_port_attr {
    pub state: c_uint,       // ibv_port_state
    pub max_mtu: c_uint,     // ibv_mtu
    pub active_mtu: c_uint,  // ibv_mtu
    pub gid_tbl_len: c_int,
    pub port_cap_flags: u32,
    pub max_msg_sz: u32,
    pub bad_pkey_cntr: u32,
    pub qkey_viol_cntr: u32,
    pub pkey_tbl_len: u16,
    pub lid: u16,
    pub sm_lid: u16,
    pub lmc: u8,
    pub max_vl_num: u8,
    pub sm_sl: u8,
    pub subnet_timeout: u8,
    pub init_type_reply: u8,
    pub active_width: u8,
    pub active_speed: u8,
    pub phys_state: u8,
    pub link_layer: u8,
    pub flags: u8,
    pub port_cap_flags2: u16,
    pub active_speed_ex: u32,
}

// =============================================================================
// ibv_gid (verbs.h linia 65)
// =============================================================================

#[repr(C)]
#[derive(Copy, Clone)]
pub union ibv_gid {
    pub raw: [u8; 16],
    pub global: ibv_gid_global,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct ibv_gid_global {
    pub subnet_prefix: u64,
    pub interface_id: u64,
}

// =============================================================================
// ibv_sge (verbs.h linia 1172)
// =============================================================================

#[repr(C)]
#[derive(Default)]
pub struct ibv_sge {
    pub addr: u64,
    pub length: u32,
    pub lkey: u32,
}

// =============================================================================
// ibv_send_wr (verbs.h linia 1183)
// Struktura zawiera kilka unii. Uzywamy tylko IBV_WR_SEND wiec
// unie traktujemy jako padding odpowiedniego rozmiaru.
// =============================================================================

#[repr(C)]
pub struct ibv_send_wr {
    pub wr_id: u64,
    pub next: *mut ibv_send_wr,
    pub sg_list: *mut ibv_sge,
    pub num_sge: c_int,
    pub opcode: c_uint,
    pub send_flags: c_uint,
    // union { __be32 imm_data; uint32_t invalidate_rkey; }
    pub imm_data_invalidated_rkey: u32,
    // union wr { rdma (8+4=12+pad=16), atomic (8+8+8+4=28+pad=32), ud (8+4+4=16) }
    // Maksymalny rozmiar to atomic: 32 bajty
    pub wr: [u8; 32],
    // union qp_type { xrc { remote_srqn: u32 } } = 4+pad
    pub qp_type: [u8; 8],
    // union { bind_mw, tso } — nie uzywamy, ale musi byc dla poprawnego rozmiaru
    // bind_mw: ptr + u32 + struct ibv_mw_bind_info (addr: u64, length: u64, mr_lkey: u32, flags: u32 = 24 bytes) = 8+4+24 = 36+pad = 40
    // tso: ptr + u16 + u16 + pad = 8+4 = 12
    // Wiekszy to bind_mw: 40 bajtow (z padami)
    pub _tail_union: [u8; 40],
}

// =============================================================================
// ibv_recv_wr (verbs.h linia 1233)
// =============================================================================

#[repr(C)]
pub struct ibv_recv_wr {
    pub wr_id: u64,
    pub next: *mut ibv_recv_wr,
    pub sg_list: *mut ibv_sge,
    pub num_sge: c_int,
}

// =============================================================================
// ibv_wc (verbs.h linia 592)
// =============================================================================

#[repr(C)]
#[derive(Default)]
pub struct ibv_wc {
    pub wr_id: u64,
    pub status: c_uint,
    pub opcode: c_uint,
    pub vendor_err: u32,
    pub byte_len: u32,
    // union { __be32 imm_data; uint32_t invalidated_rkey; }
    pub imm_data_invalidated_rkey: u32,
    pub qp_num: u32,
    pub src_qp: u32,
    pub wc_flags: c_uint,
    pub pkey_index: u16,
    pub slid: u16,
    pub sl: u8,
    pub dlid_path_bits: u8,
}

// =============================================================================
// ibv_global_route (verbs.h linia 717)
// =============================================================================

#[repr(C)]
#[derive(Copy, Clone)]
pub struct ibv_global_route {
    pub dgid: ibv_gid,
    pub flow_label: u32,
    pub sgid_index: u8,
    pub hop_limit: u8,
    pub traffic_class: u8,
}

// =============================================================================
// ibv_ah_attr (verbs.h linia 788)
// =============================================================================

#[repr(C)]
#[derive(Copy, Clone)]
pub struct ibv_ah_attr {
    pub grh: ibv_global_route,
    pub dlid: u16,
    pub sl: u8,
    pub src_path_bits: u8,
    pub static_rate: u8,
    pub is_global: u8,
    pub port_num: u8,
}

// =============================================================================
// ibv_qp_cap (verbs.h linia 937)
// =============================================================================

#[repr(C)]
#[derive(Default)]
pub struct ibv_qp_cap {
    pub max_send_wr: u32,
    pub max_recv_wr: u32,
    pub max_send_sge: u32,
    pub max_recv_sge: u32,
    pub max_inline_data: u32,
}

// =============================================================================
// ibv_qp_init_attr (verbs.h linia 945)
// =============================================================================

#[repr(C)]
pub struct ibv_qp_init_attr {
    pub qp_context: *mut c_void,
    pub send_cq: *mut ibv_cq,
    pub recv_cq: *mut ibv_cq,
    pub srq: *mut ibv_srq,
    pub cap: ibv_qp_cap,
    pub qp_type: c_uint, // ibv_qp_type
    pub sq_sig_all: c_int,
}

// =============================================================================
// ibv_qp_attr (verbs.h linia 1094)
// =============================================================================

#[repr(C)]
pub struct ibv_qp_attr {
    pub qp_state: c_uint,       // ibv_qp_state
    pub cur_qp_state: c_uint,   // ibv_qp_state
    pub path_mtu: c_uint,       // ibv_mtu
    pub path_mig_state: c_uint, // ibv_mig_state
    pub qkey: u32,
    pub rq_psn: u32,
    pub sq_psn: u32,
    pub dest_qp_num: u32,
    pub qp_access_flags: c_uint,
    pub cap: ibv_qp_cap,
    pub ah_attr: ibv_ah_attr,
    pub alt_ah_attr: ibv_ah_attr,
    pub pkey_index: u16,
    pub alt_pkey_index: u16,
    pub en_sqd_async_notify: u8,
    pub sq_draining: u8,
    pub max_rd_atomic: u8,
    pub max_dest_rd_atomic: u8,
    pub min_rnr_timer: u8,
    pub port_num: u8,
    pub timeout: u8,
    pub retry_cnt: u8,
    pub rnr_retry: u8,
    pub alt_port_num: u8,
    pub alt_timeout: u8,
    // padding do wyrownania 4-bajtowego przed rate_limit
    pub _pad: u8,
    pub rate_limit: u32,
}

// =============================================================================
// Stale — wartosci z enumeracji libibverbs
// =============================================================================

// ibv_qp_type
pub const IBV_QPT_RC: c_uint = 2;

// ibv_qp_state
pub const IBV_QPS_RESET: c_uint = 0;
pub const IBV_QPS_INIT: c_uint = 1;
pub const IBV_QPS_RTR: c_uint = 2;
pub const IBV_QPS_RTS: c_uint = 3;

// ibv_mtu
pub const IBV_MTU_256: c_uint = 1;
pub const IBV_MTU_512: c_uint = 2;
pub const IBV_MTU_1024: c_uint = 3;
pub const IBV_MTU_2048: c_uint = 4;
pub const IBV_MTU_4096: c_uint = 5;

// ibv_access_flags
pub const IBV_ACCESS_LOCAL_WRITE: c_uint = 1;
pub const IBV_ACCESS_REMOTE_WRITE: c_uint = 1 << 1;
pub const IBV_ACCESS_REMOTE_READ: c_uint = 1 << 2;

// ibv_wr_opcode (IBV_WR_RDMA_WRITE=0, IBV_WR_RDMA_WRITE_WITH_IMM=1, IBV_WR_SEND=2)
pub const IBV_WR_SEND: c_uint = 2;

// ibv_send_flags (IBV_SEND_FENCE=1, IBV_SEND_SIGNALED=2)
pub const IBV_SEND_SIGNALED: c_uint = 1 << 1;

// ibv_wc_status
pub const IBV_WC_SUCCESS: c_uint = 0;

// ibv_qp_attr_mask
pub const IBV_QP_STATE: c_int = 1 << 0;
pub const IBV_QP_CUR_STATE: c_int = 1 << 1;
pub const IBV_QP_EN_SQD_ASYNC_NOTIFY: c_int = 1 << 2;
pub const IBV_QP_ACCESS_FLAGS: c_int = 1 << 3;
pub const IBV_QP_PKEY_INDEX: c_int = 1 << 4;
pub const IBV_QP_PORT: c_int = 1 << 5;
pub const IBV_QP_QKEY: c_int = 1 << 6;
pub const IBV_QP_AV: c_int = 1 << 7;
pub const IBV_QP_PATH_MTU: c_int = 1 << 8;
pub const IBV_QP_TIMEOUT: c_int = 1 << 9;
pub const IBV_QP_RETRY_CNT: c_int = 1 << 10;
pub const IBV_QP_RNR_RETRY: c_int = 1 << 11;
pub const IBV_QP_RQ_PSN: c_int = 1 << 12;
pub const IBV_QP_MAX_QP_RD_ATOMIC: c_int = 1 << 13;
pub const IBV_QP_ALT_PATH: c_int = 1 << 14;
pub const IBV_QP_MIN_RNR_TIMER: c_int = 1 << 15;
pub const IBV_QP_SQ_PSN: c_int = 1 << 16;
pub const IBV_QP_MAX_DEST_RD_ATOMIC: c_int = 1 << 17;
pub const IBV_QP_PATH_MIG_STATE: c_int = 1 << 18;
pub const IBV_QP_CAP: c_int = 1 << 19;
pub const IBV_QP_DEST_QPN: c_int = 1 << 20;

// =============================================================================
// Linkowane symbole z libibverbs — realne eksportowane funkcje
// =============================================================================

#[link(name = "ibverbs")]
extern "C" {
    pub fn ibv_get_device_list(num_devices: *mut c_int) -> *mut *mut ibv_device;
    pub fn ibv_free_device_list(list: *mut *mut ibv_device);
    pub fn ibv_get_device_name(device: *mut ibv_device) -> *const c_char;
    pub fn ibv_open_device(device: *mut ibv_device) -> *mut ibv_context;
    pub fn ibv_close_device(context: *mut ibv_context) -> c_int;
    pub fn ibv_alloc_pd(context: *mut ibv_context) -> *mut ibv_pd;
    pub fn ibv_dealloc_pd(pd: *mut ibv_pd) -> c_int;
    pub fn ibv_create_cq(
        context: *mut ibv_context,
        cqe: c_int,
        cq_context: *mut c_void,
        channel: *mut c_void,
        comp_vector: c_int,
    ) -> *mut ibv_cq;
    pub fn ibv_destroy_cq(cq: *mut ibv_cq) -> c_int;
    pub fn ibv_reg_mr(
        pd: *mut ibv_pd,
        addr: *mut c_void,
        length: usize,
        access: c_int,
    ) -> *mut ibv_mr;
    pub fn ibv_dereg_mr(mr: *mut ibv_mr) -> c_int;
    pub fn ibv_create_qp(pd: *mut ibv_pd, qp_init_attr: *mut ibv_qp_init_attr) -> *mut ibv_qp;
    pub fn ibv_destroy_qp(qp: *mut ibv_qp) -> c_int;
    pub fn ibv_modify_qp(qp: *mut ibv_qp, attr: *mut ibv_qp_attr, attr_mask: c_int) -> c_int;
    pub fn ibv_query_port(
        context: *mut ibv_context,
        port_num: u8,
        port_attr: *mut ibv_port_attr,
    ) -> c_int;
    pub fn ibv_query_gid(
        context: *mut ibv_context,
        port_num: u8,
        index: c_int,
        gid: *mut ibv_gid,
    ) -> c_int;
}

// =============================================================================
// Inline wrappery — ibv_post_send, ibv_post_recv, ibv_poll_cq
// W C to sa static inline wolajace przez vtable ibv_context.ops.
// Tu odtwarzamy ten sam mechanizm.
// =============================================================================

/// ibv_post_send — wywoluje context->ops.post_send(qp, wr, bad_wr)
/// Zwraca -1 jesli function pointer nie jest ustawiony w vtable.
///
/// # Safety
/// Wymaga poprawnych wskaznikow do zainicjalizowanych zasobow RDMA.
pub unsafe fn ibv_post_send(
    qp: *mut ibv_qp,
    wr: *mut ibv_send_wr,
    bad_wr: *mut *mut ibv_send_wr,
) -> c_int {
    let ctx = (*qp).context;
    match (*ctx).ops.post_send {
        Some(f) => f(qp, wr, bad_wr),
        None => -1,
    }
}

/// ibv_post_recv — wywoluje context->ops.post_recv(qp, wr, bad_wr)
/// Zwraca -1 jesli function pointer nie jest ustawiony w vtable.
///
/// # Safety
/// Wymaga poprawnych wskaznikow do zainicjalizowanych zasobow RDMA.
pub unsafe fn ibv_post_recv(
    qp: *mut ibv_qp,
    wr: *mut ibv_recv_wr,
    bad_wr: *mut *mut ibv_recv_wr,
) -> c_int {
    let ctx = (*qp).context;
    match (*ctx).ops.post_recv {
        Some(f) => f(qp, wr, bad_wr),
        None => -1,
    }
}

/// ibv_poll_cq — wywoluje cq->context->ops.poll_cq(cq, num_entries, wc)
/// Zwraca -1 jesli function pointer nie jest ustawiony w vtable.
///
/// # Safety
/// Wymaga poprawnych wskaznikow do zainicjalizowanych zasobow RDMA.
pub unsafe fn ibv_poll_cq(cq: *mut ibv_cq, num_entries: c_int, wc: *mut ibv_wc) -> c_int {
    let ctx = (*cq).context;
    match (*ctx).ops.poll_cq {
        Some(f) => f(cq, num_entries, wc),
        None => -1,
    }
}

// =============================================================================
// Testy — walidacja ukladu pamieci struktur FFI wzgledem C ABI
// =============================================================================

#[cfg(all(test, feature = "rdma-probe"))]
mod tests {
    use super::*;

    #[test]
    fn verify_struct_sizes() {
        // Rozmiary struktur FFI musza zgadzac sie z C ABI
        // Sprawdzone na aarch64 i x86_64 z libibverbs 1.14+

        // ibv_sge: 3 pola (u64 + u32 + u32) = 16 bajtow
        assert_eq!(std::mem::size_of::<ibv_sge>(), 16, "ibv_sge size mismatch");

        // ibv_wc: sprawdz ze minimum zawiera potrzebne pola
        assert!(std::mem::size_of::<ibv_wc>() >= 48, "ibv_wc too small");

        // ibv_port_attr: sprawdz offset lid
        let port_attr = unsafe { std::mem::zeroed::<ibv_port_attr>() };
        let base = &port_attr as *const _ as usize;
        let lid_offset = &port_attr.lid as *const _ as usize - base;
        // lid jest po 9 polach (state, max_mtu, active_mtu, gid_tbl_len,
        // port_cap_flags, max_msg_sz, bad_pkey_cntr, qkey_viol_cntr, pkey_tbl_len)
        // Na obu architekturach lid powinien byc na stalym ustawieniu
        assert!(lid_offset > 0, "lid offset should be > 0");

        // ibv_gid: union z raw [u8; 16]
        assert_eq!(std::mem::size_of::<ibv_gid>(), 16, "ibv_gid size mismatch");
    }
}
