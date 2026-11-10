/* Derived from Linux drivers/net/wireless/ath/ath10k (SPDX ISC; Atheros Communications, Qualcomm Atheros, The Linux Foundation, Qualcomm
 * Innovation Center, Qualcomm Technologies): the extracted code is compiled unchanged, see oracle/README.md. */
/* Stand-in for ath10k's hif.h/core.h, just enough of `struct ath10k` and the kernel API for the real ce.c to
 * compile and run on the host (see oracle/ce_oracle.c). Nothing here is Linux code. */
#ifndef SHIM_HIF_H
#define SHIM_HIF_H
#include <linux/types.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
struct ath10k;
struct survey_info;
#include "hw.h"

#define MS(_v, _f) (((_v) & _f##_MASK) >> _f##_LSB)
#define SM(_v, _f) (((_v) << _f##_LSB) & _f##_MASK)

#define EXPORT_SYMBOL(x)
#define likely(x) (x)
#define unlikely(x) (x)
#define WARN_ON_ONCE(x) (x)
#define BUILD_BUG_ON(x)
#define lockdep_assert_held(x) ((void)(x))
#define spin_lock_bh(x) ((void)(x))
#define spin_unlock_bh(x) ((void)(x))
typedef int spinlock_t;
typedef unsigned gfp_t;
#define GFP_KERNEL 0
#define ERR_PTR(e) ((void *)(long)(e))
#define PTR_ERR(p) ((long)(p))
#define IS_ERR(p) ((unsigned long)(p) >= (unsigned long)-4095L)
#define lower_32_bits(n) ((u32)((n) & 0xffffffff))
#define upper_32_bits(n) ((u32)(((n) >> 16) >> 16))
#define PTR_ALIGN(p, a) ((void *)(((uintptr_t)(p) + (a) - 1) & ~((uintptr_t)(a) - 1)))
#define ALIGN(x, a) (((x) + (a) - 1) & ~((__typeof__(x))(a) - 1))
#define ath10k_dbg(...) ((void)0)
#define ath10k_warn(...) ((void)0)
#define ath10k_err(...) ((void)0)
#define ath10k_dbg_dump(...) ((void)0)
#define ATH10K_DBG_BOOT 0

enum { ATH10K_FLAG_CRASH_FLUSH = 5 };

static inline unsigned long roundup_pow_of_two(unsigned long n)
{
	unsigned long r = 1;
	while (r < n)
		r <<= 1;
	return r;
}

/* Bus addresses are handed out from a counter so the traces do not depend on where malloc put things. */
extern u64 shim_next_bus_addr;
void *shim_dma_alloc(size_t size, u64 *handle);
#define dma_alloc_coherent(dev, size, handle, gfp) shim_dma_alloc((size), (handle))
#define dma_free_coherent(dev, size, vaddr, handle) free(vaddr)
#define kzalloc_flex(P, M, N) calloc(1, sizeof(P) + (size_t)(N) * sizeof(((__typeof__(P) *)0)->M[0]))
#define kcalloc(n, sz, gfp) calloc((n), (sz))
#define kfree(p) free(p)

struct ath10k_ce_crash_data {
	__le32 base_addr, src_wr_idx, src_r_idx, dst_wr_idx, dst_r_idx;
};
struct ath10k_fw_crash_data {
	struct ath10k_ce_crash_data ce_crash_data[12];
};

struct ath10k {
	void *dev;
	unsigned long dev_flags;
	enum ath10k_hw_rev hw_rev;
	struct ath10k_hw_params hw_params;
	const struct ath10k_hw_regs *regs;
	const struct ath10k_hw_values *hw_values;
	const struct ath10k_hw_ce_regs *hw_ce_regs;
	void *ce_priv;
	int dump_mutex;
};
#endif
