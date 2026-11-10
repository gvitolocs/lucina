/* The few kernel types af_qrtr.c's structs and uapi/linux/qrtr.h use. */
#ifndef LUCINA_QRTR_ORACLE_TYPES_H
#define LUCINA_QRTR_ORACLE_TYPES_H

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
typedef uint8_t __u8;
typedef uint16_t __u16;
typedef uint32_t __u32;
typedef uint16_t __le16;
typedef uint32_t __le32;

#define __packed __attribute__((packed))
#define BIT(n) (1UL << (n))

/* The oracle host is little-endian; Linux's helpers are the identity there. */
#define cpu_to_le16(x) ((__le16)(x))
#define cpu_to_le32(x) ((__le32)(x))

#endif
