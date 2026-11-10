/*
 * Entry, early MMU and exception vectors for the LucinaOS ARMv7-A kernel (Cortex-A9, ARM state).
 *
 * Boot protocol: the Linux ARM zImage one (Documentation/arch/arm/booting.rst), so U-Boot `bootz`, kexec and
 * QEMU `-kernel` can all start it: SVC mode, MMU and D-cache off, r0 = 0, r1 = machine type, r2 = physical
 * address of the device tree (or anything when there is none). The header carries the zImage magic at 0x24 that
 * lucina-kexec checks.
 *
 * Registers kept until kernel_main:
 *   r5  DRAM base (the load address rounded down to 128 MiB, as Linux assumes for PHYS_OFFSET)
 *   r9  PA - VA of the linear map (PA = VA + r9)
 *   r10 the r2 we were given (DTB)
 *   r11 where the bootloader loaded us
 */
    .syntax unified
    .arm

    .equ KERNEL_VBASE,  0xC0000000
    .equ TEXT_OFFSET,   0x8000
    .equ IO_VBASE,      0xF0000000

    /* Short-descriptor section attributes (SCTLR.TRE = 0, AFE = 0, domain 0, AP = 01: kernel RW only). */
    .equ SECT_NORMAL,   0x140E      /* TEX 001, C, B: inner/outer write-back write-allocate */
    .equ SECT_NC,       0x1402      /* TEX 001: normal, non-cacheable (DMA buffers) */
    .equ SECT_DEVICE,   0x0416      /* TEX 000, B: shareable device, XN */

    .equ IDMAP_MIB,     16          /* identity map of the image, dropped in mm::init */
    .equ LINEAR_MIB,    512
    .equ DMA_FIRST_MIB, 16          /* DRAM + 16 MiB .. + 32 MiB is the uncached DMA pool */
    .equ DMA_MIB,       16
    .equ IO_MIB,        64

    .section .text.head, "ax"
    .global _start
_start:
    .rept 8
    mov     r0, r0
    .endr
    b       1f
    .word   0x016f2818          /* zImage magic, offset 0x24 */
    .word   0                   /* start: position independent */
    .word   __image_size        /* end - start */
    .word   0x04030201          /* little endian */
    .word   0x45454545
    .word   0                   /* no extension table */

1:  cpsid   aif
    mrs     r3, cpsr
    bic     r3, r3, #0x1f
    orr     r3, r3, #0xd3       /* SVC, IRQ and FIQ masked */
    msr     cpsr_c, r3
    mov     r10, r2

    /* MMU, D-cache and I-cache off; forget whatever the previous kernel left in the I-cache, BTB and TLB. */
    mrc     p15, 0, r3, c1, c0, 0
    bic     r3, r3, #0x5
    bic     r3, r3, #0x1000
    mcr     p15, 0, r3, c1, c0, 0
    isb
    mov     r3, #0
    mcr     p15, 0, r3, c7, c5, 0       /* ICIALLU */
    mcr     p15, 0, r3, c7, c5, 6       /* BPIALL */
    mcr     p15, 0, r3, c8, c7, 0       /* TLBIALL */
    dsb
    isb

    /* Move the image to DRAM + TEXT_OFFSET if the bootloader put it higher (QEMU: + 0x10000).
     * If the copy would overwrite the code doing it (moved by less than the image size), go through a scratch
     * copy at DRAM + 32 MiB first, as the zImage decompressor does. */
    adr     r11, _start
reloc:
    adr     r4, _start
    ldr     r5, =0xF8000000
    and     r5, r4, r5
    add     r6, r5, #TEXT_OFFSET
    cmp     r4, r6
    beq     2f
    blo     boot_hang                   /* below DRAM + 0x8000: not supported */
    ldr     r7, =__image_size
    sub     r3, r4, r6
    cmp     r3, r7
    addlo   r6, r5, #0x02000000         /* overlapping: scratch copy, then come back to reloc */
    mov     r0, r4
    mov     r1, r6
    add     r2, r4, r7
3:  ldr     r3, [r0], #4
    str     r3, [r1], #4
    cmp     r0, r2
    blo     3b
    mov     r3, #0
    mcr     p15, 0, r3, c7, c5, 0
    dsb
    isb
    sub     r3, r6, r4
    adr     r0, reloc
    add     r0, r0, r3
    bx      r0

2:  ldr     r7, =KERNEL_VBASE
    sub     r9, r5, r7

    /* Zero .bss (it holds the boot page table). */
    ldr     r0, =__bss_start
    add     r0, r0, r9
    ldr     r1, =__bss_end
    add     r1, r1, r9
    mov     r2, #0
4:  cmp     r0, r1
    strlo   r2, [r0], #4
    blo     4b

    /* Boot page table: identity map of the image, linear map of DRAM, fixed I/O window. */
    ldr     r0, =boot_l1
    add     r0, r0, r9
    lsr     r1, r5, #20
    lsr     r2, r5, #20
    mov     r3, #IDMAP_MIB
    ldr     r12, =SECT_NORMAL
    bl      map_sections

    ldr     r1, =(KERNEL_VBASE >> 20)
    lsr     r2, r5, #20
    mov     r3, #DMA_FIRST_MIB
    ldr     r12, =SECT_NORMAL
    bl      map_sections
    mov     r3, #DMA_MIB
    ldr     r12, =SECT_NC
    bl      map_sections
    mov     r3, #(LINEAR_MIB - DMA_FIRST_MIB - DMA_MIB)
    ldr     r12, =SECT_NORMAL
    bl      map_sections

    ldr     r1, =(IO_VBASE >> 20)
    mov     r2, #0
    mov     r3, #IO_MIB
    ldr     r12, =SECT_DEVICE
    bl      map_sections

    /* TTBR0 with inner WBWA (IRGN = 01: bit 6) and outer WBWA (RGN = 01) walks; TTBCR.N = 0. */
    orr     r3, r0, #0x48
    mcr     p15, 0, r3, c2, c0, 0
    mov     r3, #0
    mcr     p15, 0, r3, c2, c0, 2
    ldr     r3, =0x55555555
    mcr     p15, 0, r3, c3, c0, 0       /* DACR: every domain is a client */
    bl      inval_l1_dcache
    mov     r3, #0
    mcr     p15, 0, r3, c8, c7, 0
    mcr     p15, 0, r3, c7, c5, 0
    mcr     p15, 0, r3, c7, c5, 6
    dsb
    isb
    mrc     p15, 0, r3, c1, c0, 0
    ldr     r4, =0x1805                 /* M, C, Z (branch prediction), I */
    orr     r3, r3, r4
    ldr     r4, =0x70002002             /* A, V (low vectors via VBAR), TRE, AFE, TE off */
    bic     r3, r3, r4
    mcr     p15, 0, r3, c1, c0, 0
    isb
    ldr     pc, =high_start

boot_hang:
    wfi
    b       boot_hang

/* r0 = L1 table (PA), r1 = first VA section, r2 = first PA section, r3 = count (> 0), r12 = attributes.
 * Returns with r1 and r2 advanced past the mapped range. Clobbers r3, r7. */
map_sections:
1:  orr     r7, r12, r2, lsl #20
    str     r7, [r0, r1, lsl #2]
    add     r1, r1, #1
    add     r2, r2, #1
    subs    r3, r3, #1
    bne     1b
    bx      lr

/* Invalidate the L1 data cache by set/way (no clean: nothing valid is in it with the cache off).
 * Clobbers r0-r4, r7, r8. The A9's L2 (PL310) is outer and stays off in Phase 0. */
inval_l1_dcache:
    mov     r0, #0
    mcr     p15, 2, r0, c0, c0, 0       /* CSSELR: L1 data */
    isb
    mrc     p15, 1, r0, c0, c0, 0       /* CCSIDR */
    and     r1, r0, #7
    add     r1, r1, #4                  /* log2(line bytes) */
    ldr     r2, =0x3ff
    and     r2, r2, r0, lsr #3          /* ways - 1 */
    ldr     r3, =0x7fff
    and     r3, r3, r0, lsr #13         /* sets - 1 */
    clz     r4, r2                      /* way field shift */
1:  mov     r7, r3
2:  lsl     r8, r2, r4
    orr     r8, r8, r7, lsl r1
    mcr     p15, 0, r8, c7, c6, 2       /* DCISW */
    subs    r7, r7, #1
    bge     2b
    subs    r2, r2, #1
    bge     1b
    dsb
    bx      lr

    .ltorg

/* Running at the linked (virtual) address from here on. */
    .text
high_start:
    ldr     r0, =__exc_stack_top
    cps     #0x12                       /* IRQ */
    mov     sp, r0
    sub     r0, r0, #4096
    cps     #0x17                       /* ABT */
    mov     sp, r0
    sub     r0, r0, #4096
    cps     #0x1b                       /* UND */
    mov     sp, r0
    sub     r0, r0, #4096
    cps     #0x11                       /* FIQ */
    mov     sp, r0
    cps     #0x13                       /* SVC */
    ldr     sp, =__stack_top
    ldr     r0, =vectors
    mcr     p15, 0, r0, c12, c0, 0      /* VBAR */
    isb
    mov     r0, r10
    mov     r1, r5
    mov     r2, r11
    mov     fp, #0
    bl      kernel_main
    b       boot_hang
    .ltorg

/* Exception vectors (VBAR, 32-byte aligned). */
    .section .text.vectors, "ax"
    .align 5
    .global vectors
vectors:
    b       vec_reset
    b       vec_undef
    b       vec_svc
    b       vec_pabt
    b       vec_dabt
    b       vec_reset
    b       vec_irq
    b       vec_fiq

/* Synchronous exceptions and FIQ: save r0-r12 and the return address on the mode's stack, call
 * rust_exception(kind, spsr, frame); it returns the address to resume at (it never returns for a fatal one). */
.macro exc_entry kind, adjust
    sub     lr, lr, #\adjust
    push    {r0-r12, lr}
    mov     r0, #\kind
    mrs     r1, spsr
    mov     r2, sp
    bl      rust_exception
    str     r0, [sp, #52]
    ldm     sp!, {r0-r12, pc}^
.endm

vec_reset:  exc_entry 0, 0
vec_undef:  exc_entry 1, 4
vec_svc:    exc_entry 2, 0
vec_pabt:   exc_entry 3, 4
vec_dabt:   exc_entry 4, 8
vec_fiq:    exc_entry 7, 4

/* IRQ: handled on the SVC stack (kernel only in Phase 0), with the stack 8-byte aligned for the AAPCS call. */
vec_irq:
    sub     lr, lr, #4
    srsdb   sp!, #0x13
    cps     #0x13
    push    {r0-r3, r12, lr}
    and     r1, sp, #4
    sub     sp, sp, r1
    push    {r1, r2}
    bl      rust_irq
    pop     {r1, r2}
    add     sp, sp, r1
    pop     {r0-r3, r12, lr}
    rfeia   sp!

/* The probe instruction for the exception self-test (an UNDEFINED encoding). */
    .text
    .global undef_probe
undef_probe:
    .word   0xe7f000f0                  /* udf #0 */
    bx      lr

    .section .bss.pagetable, "aw", %nobits
    .align 14
    .global boot_l1
boot_l1:
    .space  16384
