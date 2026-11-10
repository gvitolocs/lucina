// LucinaBoot entry: the arm64 Image header, then relocation, BSS, stack and the jump to Rust.
//
// The loader (ABL on surya, QEMU's boot stub under test) jumps to offset 0 with the MMU off and
// x0 = physical address of its device tree (Linux Documentation/arch/arm64/booting.rst).

.section .text.head, "ax"
.global _head
_head:
    b       lb_start
    .long   0
    .quad   0                       // text_offset: filled in by `lucina-boot loader-image`
    .quad   0                       // image_size, BSS and stack included: idem
    .quad   0b1010                  // flags: little endian, 4 KiB pages, may be placed anywhere
    .quad   0
    .quad   0
    .quad   0
    .ascii  "ARM\x64"
    .long   0

.text
lb_start:
    msr     daifset, #0xf
    mov     x19, x0                 // device tree
    adr     x20, _head              // where we run: the link address is 0

    // Clean and invalidate our own image to the point of coherency before the first store. With the MMU off our
    // accesses bypass the caches; lines the previous stage left dirty here could otherwise be written back later
    // over relocated pointers, BSS or the stack.
    adrp    x10, __file_end
    add     x10, x10, :lo12:__file_end
    mrs     x6, ctr_el0
    ubfx    x6, x6, #16, #4         // DminLine: log2 of the line size in words
    mov     x7, #4
    lsl     x7, x7, x6
    sub     x8, x7, #1
    bic     x9, x20, x8
0:  dc      civac, x9
    add     x9, x9, x7
    cmp     x9, x10
    b.lo    0b
    dsb     sy

    // Apply the relocations: *(base + r_offset) = base + r_addend. Only R_AARCH64_RELATIVE (1027) without a
    // symbol is expected; anything else stops here, before any Rust code reads a pointer it would have fixed.
    adrp    x9, __rela_start
    add     x9, x9, :lo12:__rela_start
    adrp    x10, __rela_end
    add     x10, x10, :lo12:__rela_end
1:  cmp     x9, x10
    b.hs    2f
    ldp     x11, x12, [x9]          // r_offset, r_info
    ldr     x13, [x9, #16]          // r_addend
    add     x9, x9, #24
    cmp     w12, #1027
    b.ne    lb_hang
    lsr     x12, x12, #32
    cbnz    x12, lb_hang
    add     x13, x13, x20
    str     x13, [x20, x11]
    b       1b

    // Zero BSS (inside the file, see linker.ld). A restarted copy (lb_restart_at) needs this too: its BSS holds the
    // old copy's values.
2:  adrp    x9, __bss_start
    add     x9, x9, :lo12:__bss_start
    adrp    x10, __bss_end
    add     x10, x10, :lo12:__bss_end
3:  cmp     x9, x10
    b.hs    4f
    str     xzr, [x9], #8
    b       3b

4:  adrp    x9, __stack_top
    add     x9, x9, :lo12:__stack_top
    and     sp, x9, #~0xf           // 16-byte aligned whatever the load address
    mov     x0, x19
    mov     x1, x20
    bl      lucinaboot_main
lb_hang:
    wfe
    b       lb_hang

// lb_restart_at(new_base x0, old_base x1, bytes x2 (multiple of 8), dtb x3) -> !
//
// Copies the file part of LucinaBoot (code, data, relocations) to new_base and starts it from its header with
// x0 = dtb. The copy's entry code redoes the relocations for the new base (each write is base + addend, so data
// the old copy already relocated is simply overwritten) and zeroes its own BSS. Nothing here touches memory
// that is not a register, the source or the destination, so the caller may run on the old copy's stack.
.global lb_restart_at
lb_restart_at:
    mov     x10, x0
    mov     x4, #0
1:  cmp     x4, x2
    b.hs    2f
    ldr     x5, [x1, x4]
    str     x5, [x0, x4]
    add     x4, x4, #8
    b       1b
    // Clean and invalidate the copy to the point of coherency, then drop any stale instructions.
2:  mrs     x6, ctr_el0
    ubfx    x6, x6, #16, #4         // DminLine: log2 of the line size in words
    mov     x7, #4
    lsl     x7, x7, x6
    sub     x8, x7, #1
    bic     x5, x0, x8
    add     x9, x0, x2
3:  dc      civac, x5
    add     x5, x5, x7
    cmp     x5, x9
    b.lo    3b
    dsb     sy
    ic      iallu
    dsb     sy
    isb
    mov     x0, x3
    br      x10
