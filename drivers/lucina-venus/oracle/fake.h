/* Just enough of the kernel for Venus's hfi_cmds.c, hfi_venus.c, hfi_msgs.c and hfi_parser.c to compile as they are and run in one
 * thread. Register accesses and DMA are recorded by main.c: readl/writel go through oracle_readl/oracle_writel. */
#ifndef ORACLE_FAKE_H
#define ORACLE_FAKE_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#define ENOMEM 12
#define EBUSY 16
#define ENODEV 19
#define EINVAL 22
#define ENOSPC 28
#define ENODATA 61
#define EBADMSG 74
#define ETIMEDOUT 110
#define EINPROGRESS 115
#define ENOTSUPP 524
#define EFAULT 14
#define EIO 5
#define ERANGE 34
typedef uint8_t u8, __u8;
typedef uint16_t u16, __u16;
typedef uint32_t u32, __u32;
typedef uint64_t u64, __u64;
typedef int8_t s8, __s8;
typedef int16_t s16, __s16;
typedef int32_t s32, __s32;
typedef int64_t s64, __s64;
typedef uint16_t __le16;
typedef uint32_t __le32;
typedef uint64_t phys_addr_t, dma_addr_t, resource_size_t;
typedef long ssize_t_;
#define __packed __attribute__((packed))
#define __counted_by(x)
#define __iomem
#define __user
#define __init
#define __exit
#define __maybe_unused __attribute__((unused))
#define __always_unused __attribute__((unused))
#define __must_check
#define fallthrough __attribute__((fallthrough))
#define GFP_KERNEL 0
#define GFP_ATOMIC 0
#define BIT(n) (1UL << (n))
#define BIT_ULL(n) (1ULL << (n))
#define GENMASK(h, l) (((~0UL) << (l)) & (~0UL >> (63 - (h))))
#define ARRAY_SIZE(a) (sizeof(a) / sizeof((a)[0]))
#define lower_32_bits(n) ((u32)((n) & 0xffffffff))
#define upper_32_bits(n) ((u32)(((u64)(n)) >> 32))
#define ALIGN(x, a) (((x) + (a) - 1) & ~((__typeof__(x))(a) - 1))
#define IS_ALIGNED(x, a) (((x) & ((__typeof__(x))(a) - 1)) == 0)
#define DIV_ROUND_UP(n, d) (((n) + (d) - 1) / (d))
#define round_up(x, y) ((((x) - 1) | ((__typeof__(x))((y) - 1))) + 1)
#define SZ_4K 0x1000
#define SZ_1M 0x100000
#define SZ_2M 0x200000
#define SZ_8K 0x2000
#define SZ_16K 0x4000
#define SZ_32K 0x8000
#define SZ_64K 0x10000
#define SZ_128K 0x20000
#define SZ_256K 0x40000
#define SZ_512K 0x80000
#define PAGE_SIZE 4096
#define PAGE_ALIGN(x) ALIGN(x, PAGE_SIZE)
#define HZ 1000
#define jiffies 0UL
#define msecs_to_jiffies(m) (m)
#define usecs_to_jiffies(m) (m)
#define time_after(a, b) 0
#define hweight_long(x) __builtin_popcountl(x)
#define hweight32(x) __builtin_popcount(x)
#define min(a, b) ((a) < (b) ? (a) : (b))
#define max(a, b) ((a) > (b) ? (a) : (b))
#define min_t(t, a, b) ((t)(a) < (t)(b) ? (t)(a) : (t)(b))
#define max_t(t, a, b) ((t)(a) > (t)(b) ? (t)(a) : (t)(b))
#define clamp(v, lo, hi) min(max(v, lo), hi)
#define container_of(ptr, type, member) ((type *)((char *)(ptr) - offsetof(type, member)))
#define struct_size(p, m, n) (sizeof(*(p)) + (size_t)(n) * sizeof((p)->m[0]))
#define struct_size_t(t, m, n) (sizeof(t) + (size_t)(n) * sizeof(((t *)0)->m[0]))
#define flex_array_size(p, m, n) ((size_t)(n) * sizeof((p)->m[0]))
#define ERR_PTR(e) ((void *)(long)(e))
#define PTR_ERR(p) ((long)(p))
#define IS_ERR(p) ((unsigned long)(p) >= (unsigned long)-4095)
#define IS_ERR_OR_NULL(p) (!(p) || IS_ERR(p))
#define WARN_ON(x) (x)
#define WARN_ON_ONCE(x) (x)
#define WARN(c, ...) (c)
#define BUILD_BUG_ON(x)
#define likely(x) (x)
#define unlikely(x) (x)
#define READ_ONCE(x) (x)
#define WRITE_ONCE(x, v) ((x) = (v))
#define kzalloc(n, f) calloc(1, (n))
#define kcalloc(n, s, f) calloc((n), (s))
#define kmalloc(n, f) malloc(n)
#define kfree(p) free((void *)(p))
#define devm_kzalloc(d, n, f) calloc(1, (n))
#define kzalloc_obj(x, ...) calloc(1, sizeof(x))
#define kzalloc_objs(x, n, ...) calloc((n), sizeof(x))
#define rmb() do { } while (0)
#define wmb() do { } while (0)
#define mb() do { } while (0)
#define udelay(x) do { } while (0)
#define usleep_range(a, b) do { } while (0)
#define msleep(x) do { } while (0)
#define cond_resched() do { } while (0)
/* Session ids: hash32_ptr of the instance in Linux, a fixed number here so the bytes compare. */
#define hash32_ptr(p) 0x5e55u
typedef int spinlock_t;
struct mutex { int x; };
#define DEFINE_MUTEX(m) struct mutex m
#define mutex_init(l) (void)(l)
#define mutex_lock(l) (void)(l)
#define mutex_unlock(l) (void)(l)
#define mutex_destroy(l) (void)(l)
#define lockdep_assert_held(l) (void)(l)
#define spin_lock_init(l) (void)(l)
struct completion { int done; };
#define init_completion(c) ((c)->done = 0)
#define reinit_completion(c) ((c)->done = 0)
#define complete(c) ((c)->done = 1)
#define wait_for_completion_timeout(c, t) ((c)->done ? 1 : (fprintf(stderr, "oracle: no answer at %s:%d\n", __FILE__, __LINE__), 0))
struct list_head { struct list_head *next, *prev; };
#define LIST_HEAD(n) struct list_head n = { &(n), &(n) }
static inline void INIT_LIST_HEAD(struct list_head *h) { h->next = h->prev = h; }
static inline void list_add_tail(struct list_head *n, struct list_head *h) { n->prev = h->prev; n->next = h; h->prev->next = n; h->prev = n; }
static inline void list_del(struct list_head *n) { n->prev->next = n->next; n->next->prev = n->prev; }
static inline void list_del_init(struct list_head *n) { list_del(n); INIT_LIST_HEAD(n); }
static inline int list_empty(const struct list_head *h) { return h->next == h; }
#define list_for_each_entry(p, h, m) for (p = container_of((h)->next, __typeof__(*p), m); &p->m != (h); p = container_of(p->m.next, __typeof__(*p), m))
#define list_for_each_entry_safe(p, n, h, m) for (p = container_of((h)->next, __typeof__(*p), m), n = container_of(p->m.next, __typeof__(*p), m); &p->m != (h); p = n, n = container_of(n->m.next, __typeof__(*n), m))
#define list_first_entry(h, t, m) container_of((h)->next, t, m)
typedef int irqreturn_t;
#define IRQ_NONE 0
#define IRQ_HANDLED 1
#define IRQ_WAKE_THREAD 2
struct device { void *drvdata; };
#define dev_err(d, ...) fprintf(stderr, __VA_ARGS__)
#define dev_info(d, ...) fprintf(stderr, __VA_ARGS__)
#define dev_dbg(d, ...) do { } while (0)
#define dev_warn(d, ...) fprintf(stderr, __VA_ARGS__)
#define dev_err_ratelimited(d, ...) fprintf(stderr, __VA_ARGS__)
#define pr_err(...) fprintf(stderr, __VA_ARGS__)
#define pr_debug(...) do { } while (0)
#define print_hex_dump(...) do { } while (0)
#define EXPORT_SYMBOL_GPL(x)
#define EXPORT_SYMBOL(x)
#define MODULE_PARM_DESC(a, b)
#define module_param(a, b, c)
#define module_param_named(a, b, c, d)
#define strscpy(d, s, n) strncpy((d), (s), (n))
#endif
