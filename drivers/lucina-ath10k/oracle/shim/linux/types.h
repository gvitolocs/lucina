/* Minimal stand-ins for the kernel headers that wmi.h, wmi-tlv.h, htc.h, htt.h and ce.h need, so the wire
 * structs compile unchanged under gcc on the host. Little-endian host assumed (checked by oracle main). */
#ifndef SHIM_LINUX_TYPES_H
#define SHIM_LINUX_TYPES_H
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
#include <string.h>
typedef uint8_t u8; typedef uint16_t u16; typedef uint32_t u32; typedef uint64_t u64;
typedef int8_t s8; typedef int16_t s16; typedef int32_t s32; typedef int64_t s64;
typedef int32_t __s32;
typedef uint8_t __u8; typedef uint16_t __u16; typedef uint32_t __u32; typedef uint64_t __u64;
typedef u16 __le16; typedef u32 __le32; typedef u64 __le64;
typedef u16 __be16; typedef u32 __be32;
typedef u64 dma_addr_t;
#define __bitwise
#define __force
#define __packed __attribute__((packed))
#define __aligned(x) __attribute__((aligned(x)))
#define __counted_by(x)
#define BIT(n) (1UL << (n))
#define BIT_ULL(n) (1ULL << (n))
#define GENMASK(h,l) (((~0UL) << (l)) & (~0UL >> (63 - (h))))
#define GENMASK_ULL(h,l) (((~0ULL) << (l)) & (~0ULL >> (63 - (h))))
#define DECLARE_FLEX_ARRAY(T, n) T n[]
#define DECLARE_BITMAP(n, b) unsigned long n[((b)+63)/64]
#define cpu_to_le16(x) ((__le16)(x))
#define cpu_to_le32(x) ((__le32)(x))
#define cpu_to_le64(x) ((__le64)(x))
#define le16_to_cpu(x) ((u16)(x))
#define le32_to_cpu(x) ((u32)(x))
#define le64_to_cpu(x) ((u64)(x))
#define __cpu_to_le16 cpu_to_le16
#define __cpu_to_le32 cpu_to_le32
#define __cpu_to_le64 cpu_to_le64
#define __le16_to_cpu le16_to_cpu
#define __le32_to_cpu le32_to_cpu
#define __le64_to_cpu le64_to_cpu
#define ARRAY_SIZE(a) (sizeof(a) / sizeof((a)[0]))
#define FIELD_GET(mask, v) (((v) & (mask)) >> __builtin_ctzll(mask))
#define FIELD_PREP(mask, v) (((u64)(v) << __builtin_ctzll(mask)) & (mask))
static inline int test_bit(int nr, const unsigned long *addr) { return (addr[nr / 64] >> (nr % 64)) & 1; }
static inline void __set_bit(int nr, void *addr) { ((unsigned long *)addr)[nr / 64] |= 1UL << (nr % 64); }
#define ETH_ALEN 6
struct sk_buff;
struct sk_buff_head { int dummy; };
struct completion { int done; };
struct work_struct { int dummy; };
typedef int spinlock_t;
struct timer_list { int dummy; };
struct list_head { void *next, *prev; };
struct ath10k;
struct survey_info;
#endif
