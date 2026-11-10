/* Minimal stand-ins for the kernel headers that qmi_encdec.c, qmi.h and
 * qmi_wlfw_v01.c include. Only what those files use; nothing here changes how
 * the Linux code encodes or decodes. */
#ifndef LUCINA_QMI_ORACLE_KSHIM_H
#define LUCINA_QMI_ORACLE_KSHIM_H

#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>

typedef uint8_t u8;
typedef uint16_t u16;
typedef uint32_t u32;
typedef uint64_t u64;
typedef int32_t s32;
typedef uint16_t __le16;
typedef uint32_t __le32;
typedef uint64_t __le64;
typedef unsigned short __kernel_sa_family_t;
typedef unsigned int gfp_t;
typedef uint32_t __u32;

#define __packed __attribute__((packed))
#define U8_MAX 255
#define GFP_KERNEL 0
#define GFP_ATOMIC 1
#define ETOOSMALL 525 /* include/linux/errno.h */

/* The oracle host is little-endian; Linux's helpers are the identity there. */
#define __cpu_to_le16(x) ((__le16)(x))
#define __cpu_to_le32(x) ((__le32)(x))
#define __cpu_to_le64(x) ((__le64)(x))
#define __le16_to_cpu(x) ((u16)(x))
#define __le32_to_cpu(x) ((u32)(x))
#define __le64_to_cpu(x) ((u64)(x))
#define cpu_to_le16(x) __cpu_to_le16(x)
#define cpu_to_le32(x) __cpu_to_le32(x)
#define le16_to_cpu(x) __le16_to_cpu(x)
#define le32_to_cpu(x) __le32_to_cpu(x)

#define pr_err(...) fprintf(stderr, "qmi-oracle: " __VA_ARGS__)
#define ERR_PTR(e) ((void *)(long)(e))
#define IS_ERR(p) ((unsigned long)(void *)(p) >= (unsigned long)-4095)
#define PTR_ERR(p) ((long)(p))
#define kzalloc(sz, fl) calloc(1, (sz))
#define kfree(p) free(p)

#define EXPORT_SYMBOL_GPL(x)
#define MODULE_DESCRIPTION(x)
#define MODULE_LICENSE(x)

/* Opaque members of struct qmi_handle and struct qmi_txn: never touched. */
struct completion { int unused; };
struct idr { int unused; };
struct mutex { int unused; };
struct list_head { struct list_head *next, *prev; };
struct work_struct { int unused; };
struct workqueue_struct;

#endif
