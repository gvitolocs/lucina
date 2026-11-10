/* Just enough of the kernel for q6afe.c, q6adm.c, q6asm.c, q6core.c and q6dsp-common.c to compile as they are and run in one thread.
 * The wait macros need no waiting: apr_send_pkt in main.c answers from inside the call. */
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
#define ETIMEDOUT 110
#define ENOTSUPP 524
typedef uint8_t u8, __u8;
typedef uint16_t u16, __u16;
typedef uint32_t u32, __u32;
typedef uint64_t u64, __u64;
typedef int8_t s8, __s8;
typedef int16_t s16, __s16;
typedef int32_t s32, __s32;
typedef int64_t s64, __s64;
typedef uint16_t __le16, __bitwise16;
typedef uint32_t __le32;
typedef uint64_t phys_addr_t, dma_addr_t;
#define APR_NAME_SIZE 32
struct apr_device_id { char name[APR_NAME_SIZE]; u32 domain_id; u32 svc_id; u32 svc_version; unsigned long driver_data; };
#define __packed __attribute__((packed))
#define __user
#define __init
#define __exit
#define __always_unused __attribute__((unused))
#define GFP_KERNEL 0
#define GFP_ATOMIC 0
#define BIT(n) (1UL << (n))
#define GENMASK(h, l) (((~0UL) << (l)) & (~0UL >> (63 - (h))))
#define ARRAY_SIZE(a) (sizeof(a) / sizeof((a)[0]))
#define lower_32_bits(n) ((u32)((n) & 0xffffffff))
#define upper_32_bits(n) ((u32)(((u64)(n)) >> 32))
#define ALIGN(x, a) (((x) + (a) - 1) & ~((__typeof__(x))(a) - 1))
#define PAGE_SIZE 4096
#define PAGE_ALIGN(x) ALIGN(x, PAGE_SIZE)
#define HZ 1000
#define jiffies 0UL
#define msecs_to_jiffies(m) (m)
#define time_after(a, b) 0
#define hweight_long(x) __builtin_popcountl(x)
#define min(a, b) ((a) < (b) ? (a) : (b))
#define max(a, b) ((a) > (b) ? (a) : (b))
#define container_of(ptr, type, member) ((type *)((char *)(ptr) - offsetof(type, member)))
#define struct_size(p, m, n) (sizeof(*(p)) + (size_t)(n) * sizeof((p)->m[0]))
#define ERR_PTR(e) ((void *)(long)(e))
#define PTR_ERR(p) ((long)(p))
#define IS_ERR(p) ((unsigned long)(p) >= (unsigned long)-4095)
#define IS_ERR_OR_NULL(p) (!(p) || IS_ERR(p))
#define ERR_CAST(p) ((void *)(p))
#define WARN_ON(x) (x)
#define likely(x) (x)
#define unlikely(x) (x)
#define kzalloc(n, f) calloc(1, (n))
#define kfree(p) free((void *)(p))
#define devm_kzalloc(d, n, f) calloc(1, (n))
#define kzalloc_obj(x, ...) calloc(1, sizeof(x))
#define kzalloc_objs(x, n, ...) calloc((n), sizeof(x))
static inline void *kmemdup(const void *p, size_t n, int f) { void *q = malloc(n); memcpy(q, p, n); return q; }
static inline void __free_kfree(void *p) { free(*(void **)p); }
#define __free(x) __attribute__((cleanup(__free_##x)))
#define guard(x) (void)
#define scoped_guard(...)
typedef int spinlock_t;
struct mutex { int x; };
typedef int wait_queue_head_t;
#define spin_lock_init(l) (void)(l)
#define spin_lock(l) (void)(l)
#define spin_unlock(l) (void)(l)
#define spin_lock_irqsave(l, f) do { (void)(l); (f) = 0; } while (0)
#define spin_unlock_irqrestore(l, f) do { (void)(l); (void)(f); } while (0)
#define mutex_init(l) (void)(l)
#define mutex_lock(l) (void)(l)
#define mutex_unlock(l) (void)(l)
#define init_waitqueue_head(w) (void)(w)
#define wake_up(w) (void)(w)
#define wait_event_timeout(w, cond, t) ((cond) ? 1 : (fprintf(stderr, "oracle: no answer at %s:%d\n", __FILE__, __LINE__), 0))
#define wait_event_interruptible(w, cond) 0
typedef struct { int counter; } atomic_t;
#define atomic_set(a, v) ((a)->counter = (v))
#define atomic_read(a) ((a)->counter)
struct kref { int count; };
static inline void kref_init(struct kref *k) { k->count = 1; }
static inline void kref_get(struct kref *k) { k->count++; }
static inline int kref_put(struct kref *k, void (*release)(struct kref *)) { if (--k->count == 0) { release(k); return 1; } return 0; }
struct list_head { struct list_head *next, *prev; };
static inline void INIT_LIST_HEAD(struct list_head *h) { h->next = h->prev = h; }
static inline void list_add_tail(struct list_head *n, struct list_head *h) { n->prev = h->prev; n->next = h; h->prev->next = n; h->prev = n; }
static inline void list_del(struct list_head *n) { n->prev->next = n->next; n->next->prev = n->prev; }
#define list_for_each_entry(p, h, m) for (p = container_of((h)->next, __typeof__(*p), m); &p->m != (h); p = container_of(p->m.next, __typeof__(*p), m))
static inline void set_bit(int n, unsigned long *a) { *a |= 1UL << n; }
static inline void clear_bit(int n, unsigned long *a) { *a &= ~(1UL << n); }
static inline int find_first_zero_bit(unsigned long *a, int n) { for (int i = 0; i < n; i++) if (!(*a & (1UL << i))) return i; return n; }
struct device_driver { const char *name; const void *of_match_table; };
struct device_node { int x; };
struct device { struct device *parent; void *drvdata; struct device_node *of_node; void *bus; void (*release)(struct device *); struct device_driver *driver; };
static inline void *dev_get_drvdata(const struct device *d) { return d->drvdata; }
static inline void dev_set_drvdata(struct device *d, void *p) { d->drvdata = p; }
#define dev_err(d, ...) fprintf(stderr, __VA_ARGS__)
#define dev_info(d, ...) fprintf(stderr, __VA_ARGS__)
#define dev_dbg(d, ...) do { } while (0)
#define dev_warn(d, ...) fprintf(stderr, __VA_ARGS__)
#define pr_err(...) fprintf(stderr, __VA_ARGS__)
#define devm_of_platform_populate(d) 0
#define of_platform_depopulate(d) do { } while (0)
struct of_device_id { const char *compatible; };
#define MODULE_DEVICE_TABLE(a, b)
#define MODULE_DESCRIPTION(x)
#define MODULE_LICENSE(x)
#define EXPORT_SYMBOL_GPL(x)
#define EXPORT_SYMBOL(x)
#define module_driver(...)
#define of_match_ptr(x) NULL
/* As the kernel's: at most n - 1 bytes and a NUL. */
static inline long strscpy(char *d, const char *s, size_t n) { size_t i = 0; if (!n) return -7; for (; i + 1 < n && s[i]; i++) d[i] = s[i]; d[i] = 0; return s[i] ? -7 : (long)i; }
#define SNDRV_PCM_STREAM_PLAYBACK 0
#define SNDRV_PCM_STREAM_CAPTURE 1
#define SND_SOC_DAIFMT_CLOCK_PROVIDER_MASK 0xf000
#define SND_SOC_DAIFMT_BP_FP (4 << 12)
#define SND_SOC_DAIFMT_BC_FC (1 << 12)
#endif
