// ===== File: shim.c — punkt koncowy RC nad libibverbs =====
//
// Rust dostaje stad uchwyt i cztery czasowniki: zarejestruj pamiec, wymien
// adresy, zapisz, odpytaj. Cale ustawianie kolejki (`ibv_modify_qp` i jego
// duze, wersjonowane struktury) zostaje tutaj, bo `verbs.h` jest jedynym
// zrodlem prawdy o ukladzie tych struktur.
//
// Tryb: RC + RDMA WRITE. Na GB10 pamiec GPU JEST pamiecia hosta (`integrated`),
// wiec rejestrujemy zwykle strony i nie ma tu sciezki GPUDirect ani zaleznosci
// od `nvidia-peermem` — nie ma osobnej pamieci karty, do ktorej trzeba by
// siegac.
//
// RoCE v2 wymaga GID-a opartego na IPv4; szukamy go po typie, a nie po stalym
// indeksie, bo numeracja GID-ow zalezy od konfiguracji interfejsu.

#include <infiniband/verbs.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>

struct forge_rdma_ep {
    struct ibv_context *ctx;
    struct ibv_pd *pd;
    struct ibv_cq *cq;
    struct ibv_qp *qp;
    uint8_t port;
    int gid_index;
    union ibv_gid gid;
    uint16_t lid;
};

// Adres, ktory druga strona musi poznac, zeby sie polaczyc i pisac.
struct forge_rdma_addr {
    uint32_t qpn;
    uint16_t lid;
    uint8_t gid[16];
    uint64_t remote_buf;
    uint32_t rkey;
};

// RoCE v2 uzywa GID-a typu IPv4; indeks nie jest staly, wiec go szukamy.
static int find_roce_v2_gid(struct ibv_context *ctx, uint8_t port) {
    struct ibv_port_attr pattr;
    if (ibv_query_port(ctx, port, &pattr)) return -1;
    for (int i = 0; i < pattr.gid_tbl_len; i++) {
        struct ibv_gid_entry entry;
        if (ibv_query_gid_ex(ctx, port, i, &entry, 0)) continue;
        if (entry.gid_type == IBV_GID_TYPE_ROCE_V2 &&
            entry.gid.global.subnet_prefix == 0 &&
            (entry.gid.raw[10] == 0xff && entry.gid.raw[11] == 0xff)) {
            return i;  // IPv4-mapped, czyli ten, ktory odpowiada adresowi IPv4
        }
    }
    return -1;
}

struct forge_rdma_ep *forge_rdma_open(const char *dev_name, uint8_t port) {
    int n = 0;
    struct ibv_device **list = ibv_get_device_list(&n);
    if (!list) return NULL;
    struct ibv_device *dev = NULL;
    for (int i = 0; i < n; i++) {
        if (!dev_name || strcmp(ibv_get_device_name(list[i]), dev_name) == 0) {
            dev = list[i];
            break;
        }
    }
    if (!dev) { ibv_free_device_list(list); return NULL; }

    struct forge_rdma_ep *ep = calloc(1, sizeof(*ep));
    if (!ep) { ibv_free_device_list(list); return NULL; }
    ep->port = port;
    ep->ctx = ibv_open_device(dev);
    ibv_free_device_list(list);
    if (!ep->ctx) { free(ep); return NULL; }

    ep->pd = ibv_alloc_pd(ep->ctx);
    if (!ep->pd) goto fail;
    ep->cq = ibv_create_cq(ep->ctx, 256, NULL, NULL, 0);
    if (!ep->cq) goto fail;

    struct ibv_qp_init_attr qia;
    memset(&qia, 0, sizeof(qia));
    qia.send_cq = ep->cq;
    qia.recv_cq = ep->cq;
    qia.qp_type = IBV_QPT_RC;
    qia.cap.max_send_wr = 128;
    qia.cap.max_recv_wr = 128;
    qia.cap.max_send_sge = 1;
    qia.cap.max_recv_sge = 1;
    ep->qp = ibv_create_qp(ep->pd, &qia);
    if (!ep->qp) goto fail;

    struct ibv_port_attr pattr;
    if (ibv_query_port(ep->ctx, port, &pattr)) goto fail;
    ep->lid = pattr.lid;
    ep->gid_index = find_roce_v2_gid(ep->ctx, port);
    if (ep->gid_index < 0) goto fail;
    if (ibv_query_gid(ep->ctx, port, ep->gid_index, &ep->gid)) goto fail;

    struct ibv_qp_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.qp_state = IBV_QPS_INIT;
    attr.pkey_index = 0;
    attr.port_num = port;
    attr.qp_access_flags = IBV_ACCESS_LOCAL_WRITE | IBV_ACCESS_REMOTE_WRITE |
                           IBV_ACCESS_REMOTE_READ;
    if (ibv_modify_qp(ep->qp, &attr,
                      IBV_QP_STATE | IBV_QP_PKEY_INDEX | IBV_QP_PORT |
                      IBV_QP_ACCESS_FLAGS))
        goto fail;
    return ep;

fail:
    if (ep->qp) ibv_destroy_qp(ep->qp);
    if (ep->cq) ibv_destroy_cq(ep->cq);
    if (ep->pd) ibv_dealloc_pd(ep->pd);
    if (ep->ctx) ibv_close_device(ep->ctx);
    free(ep);
    return NULL;
}

void forge_rdma_local_addr(struct forge_rdma_ep *ep, struct forge_rdma_addr *out) {
    out->qpn = ep->qp->qp_num;
    out->lid = ep->lid;
    memcpy(out->gid, ep->gid.raw, 16);
}

struct ibv_mr *forge_rdma_reg(struct forge_rdma_ep *ep, void *buf, size_t len) {
    return ibv_reg_mr(ep->pd, buf, len,
                      IBV_ACCESS_LOCAL_WRITE | IBV_ACCESS_REMOTE_WRITE |
                      IBV_ACCESS_REMOTE_READ);
}

uint32_t forge_rdma_rkey(struct ibv_mr *mr) { return mr->rkey; }
uint32_t forge_rdma_lkey(struct ibv_mr *mr) { return mr->lkey; }

int forge_rdma_connect(struct forge_rdma_ep *ep, const struct forge_rdma_addr *peer) {
    struct ibv_qp_attr attr;
    memset(&attr, 0, sizeof(attr));
    attr.qp_state = IBV_QPS_RTR;
    attr.path_mtu = IBV_MTU_4096;
    attr.dest_qp_num = peer->qpn;
    attr.rq_psn = 0;
    attr.max_dest_rd_atomic = 1;
    attr.min_rnr_timer = 12;
    attr.ah_attr.is_global = 1;      // RoCE zawsze routuje po GID
    attr.ah_attr.dlid = peer->lid;
    attr.ah_attr.sl = 0;
    attr.ah_attr.src_path_bits = 0;
    attr.ah_attr.port_num = ep->port;
    memcpy(attr.ah_attr.grh.dgid.raw, peer->gid, 16);
    attr.ah_attr.grh.sgid_index = ep->gid_index;
    attr.ah_attr.grh.hop_limit = 64;
    attr.ah_attr.grh.traffic_class = 0;
    if (ibv_modify_qp(ep->qp, &attr,
                      IBV_QP_STATE | IBV_QP_AV | IBV_QP_PATH_MTU |
                      IBV_QP_DEST_QPN | IBV_QP_RQ_PSN |
                      IBV_QP_MAX_DEST_RD_ATOMIC | IBV_QP_MIN_RNR_TIMER))
        return -1;

    memset(&attr, 0, sizeof(attr));
    attr.qp_state = IBV_QPS_RTS;
    attr.timeout = 14;
    attr.retry_cnt = 7;
    attr.rnr_retry = 7;
    attr.sq_psn = 0;
    attr.max_rd_atomic = 1;
    if (ibv_modify_qp(ep->qp, &attr,
                      IBV_QP_STATE | IBV_QP_TIMEOUT | IBV_QP_RETRY_CNT |
                      IBV_QP_RNR_RETRY | IBV_QP_SQ_PSN | IBV_QP_MAX_QP_RD_ATOMIC))
        return -1;
    return 0;
}

// RDMA WRITE bez powiadamiania zdalnego CPU. `signaled` steruje tym, czy
// ukonczenie trafi do kolejki — przy strumieniu zapisow sygnalizuje sie co
// n-ty, zeby nie zapychac CQ.
int forge_rdma_write(struct forge_rdma_ep *ep, struct ibv_mr *local, uint64_t local_off,
                     uint64_t remote_addr, uint32_t rkey, uint32_t len,
                     uint64_t wr_id, int signaled) {
    struct ibv_sge sge;
    sge.addr = (uint64_t)local->addr + local_off;
    sge.length = len;
    sge.lkey = local->lkey;

    struct ibv_send_wr wr, *bad = NULL;
    memset(&wr, 0, sizeof(wr));
    wr.wr_id = wr_id;
    wr.sg_list = &sge;
    wr.num_sge = 1;
    wr.opcode = IBV_WR_RDMA_WRITE;
    wr.send_flags = signaled ? IBV_SEND_SIGNALED : 0;
    wr.wr.rdma.remote_addr = remote_addr;
    wr.wr.rdma.rkey = rkey;
    return ibv_post_send(ep->qp, &wr, &bad);
}

// Zwraca liczbe ukonczen; ujemna wartosc to blad, a `status` pierwszego
// niezerowego ukonczenia trafia do `out_status`.
int forge_rdma_poll(struct forge_rdma_ep *ep, int max, int *out_status) {
    struct ibv_wc wc[64];
    if (max > 64) max = 64;
    int n = ibv_poll_cq(ep->cq, max, wc);
    *out_status = 0;
    for (int i = 0; i < n; i++) {
        if (wc[i].status != IBV_WC_SUCCESS) { *out_status = wc[i].status; break; }
    }
    return n;
}

void forge_rdma_close(struct forge_rdma_ep *ep) {
    if (!ep) return;
    if (ep->qp) ibv_destroy_qp(ep->qp);
    if (ep->cq) ibv_destroy_cq(ep->cq);
    if (ep->pd) ibv_dealloc_pd(ep->pd);
    if (ep->ctx) ibv_close_device(ep->ctx);
    free(ep);
}
