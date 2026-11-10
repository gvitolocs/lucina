/* Derived from Linux drivers/net/wireless/ath/ath10k (SPDX ISC; Atheros Communications, Qualcomm Atheros, The Linux Foundation, Qualcomm
 * Innovation Center, Qualcomm Technologies): the extracted code is compiled unchanged, see oracle/README.md. */
/* Runs the real ce.c (compiled unchanged, against a stand-in struct ath10k, see shim/hif.h) on a fake WCN3990 copy
 * engine block and prints every register access and descriptor it produces. The Rust crate replays the same
 * scenarios and must produce the same lines. */
#include <linux/types.h>
#include "hw.h"
#include "hw_wcn3990.h"
#include "ce.h"
#include "htc_svc.h"
#include "shadow_cfg.h"
#include "ce.c"

u64 shim_next_bus_addr = 0x200000000ULL;
void *shim_dma_alloc(size_t size, u64 *handle)
{
	void *p = aligned_alloc(16, (size + 15) & ~15UL);
	memset(p, 0, size);
	*handle = shim_next_bus_addr;
	shim_next_bus_addr += 0x10000;
	return p;
}

static void log_cb(const char *what, struct ath10k_ce_pipe *p) { printf("CB %s ce=%u\n", what, p->id); }
static void ath10k_snoc_htc_tx_cb(struct ath10k_ce_pipe *p) { log_cb("htc_tx", p); }
static void ath10k_snoc_htt_tx_cb(struct ath10k_ce_pipe *p) { log_cb("htt_tx", p); }
static void ath10k_snoc_htc_rx_cb(struct ath10k_ce_pipe *p) { log_cb("htc_rx", p); }
static void ath10k_snoc_htt_rx_cb(struct ath10k_ce_pipe *p) { log_cb("htt_rx", p); }
static void ath10k_snoc_htt_htc_rx_cb(struct ath10k_ce_pipe *p) { log_cb("htt_htc_rx", p); }
static void ath10k_snoc_pktlog_rx_cb(struct ath10k_ce_pipe *p) { log_cb("pktlog_rx", p); }
#include "snoc_tables.h"

static u32 regfile[0x800000 / 4];
static u32 bus_read(struct ath10k *ar, u32 off)
{
	u32 v = regfile[off / 4];
	printf("R %06x -> %08x\n", off, v);
	return v;
}
static void bus_write(struct ath10k *ar, u32 off, u32 v)
{
	printf("W %06x = %08x\n", off, v);
	regfile[off / 4] = v;
}
static const struct ath10k_bus_ops bus_ops = {.read32 = bus_read, .write32 = bus_write};

static struct ath10k ar;
static struct ath10k_ce ce;

static void setup(void)
{
	memset(&ar, 0, sizeof(ar));
	memset(&ce, 0, sizeof(ce));
	memset(regfile, 0, sizeof(regfile));
	shim_next_bus_addr = 0x200000000ULL;
	ar.hw_rev = ATH10K_HW_WCN3990;
	ar.hw_params.target_64bit = true;
	ar.hw_params.shadow_reg_support = true;
	ar.hw_params.rri_on_ddr = true;
	ar.regs = &wcn3990_regs;
	ar.hw_values = &wcn3990_values;
	ar.hw_ce_regs = &wcn3990_ce_regs;
	ar.ce_priv = &ce;
	ce.bus_ops = &bus_ops;
}

static void preset_regs(void)
{
	int id;
	for (id = 0; id < 12; id++) {
		u32 b = 0x240000 + 0x1000 * id;
		regfile[(b + 0x18) / 4] = 0x00012345 + id; /* ctrl1 */
		regfile[(b + 0x2c) / 4] = 0x2;              /* host_ie */
		regfile[(b + 0x34) / 4] = 0x3ff;            /* misc_ie */
		regfile[(b + 0x10) / 4] = 0x1f;             /* dr_base_addr_hi */
		regfile[(b + 0x4c) / 4] = 0xa5a55a5a;       /* src wm */
		regfile[(b + 0x50) / 4] = 0x5a5aa5a5;       /* dst wm */
		regfile[(b + 0x3c) / 4] = 2 + id;           /* src write index */
		regfile[(b + 0x40) / 4] = 7;                /* dst write index */
		regfile[(b + 0x44) / 4] = 2 + id;           /* current srri */
		regfile[(b + 0x48) / 4] = 7;                /* current drri */
	}
}

static void dump_pipe(unsigned id)
{
	struct ath10k_ce_pipe *p = &ce.ce_states[id];
	printf("STATE ce=%u ctrl=%06x attr=%u src_sz_max=%u", id, p->ctrl_addr, p->attr_flags, p->src_sz_max);
	if (p->src_ring)
		printf(" src[n=%u sw=%u wr=%u hw=%u base=%llx]", p->src_ring->nentries, p->src_ring->sw_index,
		       p->src_ring->write_index, p->src_ring->hw_index, (unsigned long long)p->src_ring->base_addr_ce_space);
	if (p->dest_ring)
		printf(" dst[n=%u sw=%u wr=%u base=%llx]", p->dest_ring->nentries, p->dest_ring->sw_index,
		       p->dest_ring->write_index, (unsigned long long)p->dest_ring->base_addr_ce_space);
	printf("\n");
}

static void dump_desc(const char *tag, unsigned id, void *base, unsigned idx)
{
	const uint8_t *d = (const uint8_t *)base + 16 * idx;
	int i;
	printf("DESC %s ce=%u idx=%u ", tag, id, idx);
	for (i = 0; i < 16; i++)
		printf("%02x", d[i]);
	printf("\n");
}

static void init_all(void)
{
	unsigned id;
	setup();
	preset_regs();
	printf("== alloc_rri\n");
	ath10k_ce_alloc_rri(&ar);
	/* the target keeps RRI memory in step with its own read indices: srri = 2 + id, drri = 7 */
	for (id = 0; id < 12; id++)
		ce.vaddr_rri[id] = (7u << 16) | (2 + id);
	printf("== alloc_pipe\n");
	for (id = 0; id < 12; id++) {
		int r = ath10k_ce_alloc_pipe(&ar, id, &host_ce_config_wlan[id]);
		printf("alloc ce=%u ret=%d\n", id, r);
	}
	printf("== init_pipe\n");
	for (id = 0; id < 12; id++) {
		int r = ath10k_ce_init_pipe(&ar, id, &host_ce_config_wlan[id]);
		printf("init ce=%u ret=%d\n", id, r);
		dump_pipe(id);
	}
}

int main(void)
{
	unsigned id;
	int i, r;
	void *ctx;
	unsigned nbytes;

	printf("rri_paddr %llx\n", 0ULL);
	init_all();
	printf("rri.paddr %llx\n", (unsigned long long)ce.paddr_rri);
	printf("== enable_interrupts\n");
	ath10k_ce_enable_interrupts(&ar);

	/* ---- send on CE3 (not DIS_INTR): sw_index for the full check comes from RRI memory (zero) ---- */
	printf("== send_ce3\n");
	{
		static const struct { u64 addr; unsigned n, id, flags; } t[] = {
			{0x123456780ULL, 100, 0x001, 0},
			{0x7fffff000ULL, 2048, 0xfff, CE_SEND_FLAG_BYTE_SWAP},
			{0x200000040ULL, 64, 0x12, CE_SEND_FLAG_GATHER},
			{0x000001000ULL, 1, 0, CE_SEND_FLAG_GATHER | CE_SEND_FLAG_BYTE_SWAP},
			{0x300000008ULL, 7, 5, 0},
		};
		struct ath10k_ce_pipe *p = &ce.ce_states[3];
		for (i = 0; i < 5; i++) {
			unsigned w = p->src_ring->write_index;
			r = ath10k_ce_send_nolock(p, (void *)(unsigned long)(100 + i), t[i].addr, t[i].n, t[i].id, t[i].flags);
			printf("send ret=%d\n", r);
			dump_desc("src", 3, p->src_ring->base_addr_owner_space, w);
		}
		dump_pipe(3);
		regfile[(0x240000 + 0x1000 * 3 + 0x44) / 4] = 8;
		printf("-- completed_send (srri=8)\n");
		do {
			ctx = (void *)99;
			r = ath10k_ce_completed_send_next_nolock(p, &ctx);
			printf("completed_send ret=%d ctx=%lu\n", r, (unsigned long)ctx);
		} while (r == 0);
		regfile[(0x240000 + 0x1000 * 3 + 0x44) / 4] = 0xffffffff;
		p->src_ring->hw_index = p->src_ring->sw_index;
		r = ath10k_ce_completed_send_next_nolock(p, &ctx);
		printf("completed_send srri=ffffffff ret=%d\n", r);
		dump_pipe(3);
	}

	/* ---- send on CE4 (DIS_INTR): completion reads RRI memory instead of the register ---- */
	printf("== send_ce4\n");
	{
		struct ath10k_ce_pipe *p = &ce.ce_states[4];
		for (i = 0; i < 3; i++) {
			unsigned w = p->src_ring->write_index;
			r = ath10k_ce_send_nolock(p, (void *)(unsigned long)(200 + i), 0x100000000ULL + 0x100 * i, 50 + i, i, 0);
			printf("send ret=%d\n", r);
			dump_desc("src", 4, p->src_ring->base_addr_owner_space, w);
		}
		dump_pipe(4);
		ce.vaddr_rri[4] = 0x00070000 | 9; /* srri 9: the target consumed all three (sw_index started at 6) */
		do {
			ctx = (void *)99;
			r = ath10k_ce_completed_send_next_nolock(p, &ctx);
			printf("completed_send ret=%d ctx=%lu\n", r, (unsigned long)ctx);
		} while (r == 0);
		dump_pipe(4);
	}

	/* ---- full source ring (CE0, 16 entries): the full check follows RRI memory ---- */
	printf("== send_full_ce0\n");
	{
		struct ath10k_ce_pipe *p = &ce.ce_states[0];
		int ok = 0;
		for (i = 0; i < 20; i++) {
			r = ath10k_ce_send_nolock(p, (void *)(unsigned long)i, 0x1000 + i, 4, i, 0);
			if (r == 0) ok++;
			else printf("send %d ret=%d\n", i, r);
		}
		printf("sent_ok=%d\n", ok);
		dump_pipe(0);
		ce.vaddr_rri[0] = 8;
		for (i = 0; i < 12; i++) {
			r = ath10k_ce_send_nolock(p, (void *)(unsigned long)(30 + i), 0x2000 + i, 4, i, 0);
			if (r) printf("send %d ret=%d\n", i, r);
		}
		dump_pipe(0);
		dump_desc("src", 0, p->src_ring->base_addr_owner_space, 15);
		dump_desc("src", 0, p->src_ring->base_addr_owner_space, 0);
	}

	/* ---- receive on CE2 (64 entries) ---- */
	printf("== recv_ce2\n");
	{
		struct ath10k_ce_pipe *p = &ce.ce_states[2];
		struct ce_desc_64 *base = p->dest_ring->base_addr_owner_space;
		unsigned first = p->dest_ring->write_index;
		int round;
		for (i = 0; i < 5; i++) {
			r = ath10k_ce_rx_post_buf(p, (void *)(unsigned long)(300 + i), 0x180000000ULL + 0x800ULL * i);
			printf("post ret=%d\n", r);
			dump_desc("dst", 2, base, (p->dest_ring->write_index + 63) & 63);
		}
		base[first].nbytes = 60;
		base[(first + 1) & 63].nbytes = 1500;
		base[(first + 2) & 63].nbytes = 0xffff;
		do {
			ctx = (void *)99; nbytes = 0;
			r = ath10k_ce_completed_recv_next_nolock(p, &ctx, &nbytes);
			printf("completed_recv ret=%d ctx=%lu nbytes=%u\n", r, (unsigned long)ctx, nbytes);
		} while (r == 0);
		dump_pipe(2);
		/* fill, drain, wrap */
		for (round = 0; round < 3; round++) {
			int posted = 0, k;
			while (1) {
				r = ath10k_ce_rx_post_buf(p, (void *)(unsigned long)(1000 + posted), 0x190000000ULL + 0x40ULL * posted);
				if (r) { printf("post stop ret=%d after %d\n", r, posted); break; }
				posted++;
			}
			dump_pipe(2);
			for (k = 0; k < posted; k++)
				base[(p->dest_ring->sw_index + k) & 63].nbytes = 100 + k;
			k = 0;
			do {
				ctx = 0; nbytes = 0;
				r = ath10k_ce_completed_recv_next_nolock(p, &ctx, &nbytes);
				if (r == 0) k++;
			} while (r == 0);
			printf("drained=%d last_ctx=%lu\n", k, (unsigned long)ctx);
			dump_pipe(2);
		}
		printf("-- rx_update_write_idx\n");
		regfile[(0x240000 + 0x1000 * 2 + 0x40) / 4] = 60;
		ath10k_ce_rx_update_write_idx(p, 3);
		dump_pipe(2);
		ath10k_ce_rx_update_write_idx(p, 10);
		dump_pipe(2);
	}

	/* ---- interrupt handling ---- */
	printf("== service\n");
	ath10k_ce_per_engine_service(&ar, 1);
	ath10k_ce_per_engine_service(&ar, 3);
	ath10k_ce_per_engine_service(&ar, 6);
	regfile[0x24c000 / 4] = (1u << 12) | (1u << 15) | (1u << 23);
	ath10k_ce_per_engine_service_any(&ar);
	printf("== interrupts\n");
	ath10k_ce_disable_interrupt(&ar, 1);
	ath10k_ce_disable_interrupt(&ar, 4);
	ath10k_ce_enable_interrupt(&ar, 4);
	ath10k_ce_enable_interrupt(&ar, 1);
	ath10k_ce_enable_interrupt(&ar, 6);
	ath10k_ce_disable_interrupts(&ar);

	printf("== deinit\n");
	for (id = 0; id < 12; id++)
		ath10k_ce_deinit_pipe(&ar, id);
	return 0;
}
