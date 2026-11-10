/* LucinaBoot is linked at 0 and runs wherever it is loaded: the entry code adds the load address to every
 * R_AARCH64_RELATIVE relocation. BSS and the stack are placed inside the file (as zeros, which gzip removes), so
 * LucinaBoot writes nothing beyond the bytes ABL loaded until its memory plan has checked where things are: ABL
 * does not read image_size and may put its device tree or the ramdisk right after the file. */
ENTRY(_head)

SECTIONS
{
    . = 0;
    _head_start = .;
    .text : {
        KEEP(*(.text.head))
        *(.text .text.*)
    }
    .rodata : ALIGN(8) {
        *(.rodata .rodata.*)
    }
    .data.rel.ro : ALIGN(8) {
        *(.data.rel.ro .data.rel.ro.*)
    }
    .dynamic : ALIGN(8) { *(.dynamic) }
    .dynsym : ALIGN(8) { *(.dynsym) }
    .dynstr : { *(.dynstr) }
    .hash : ALIGN(8) { *(.hash) }
    .gnu.hash : ALIGN(8) { *(.gnu.hash) }
    .got : ALIGN(8) { *(.got .got.*) }
    .data : ALIGN(8) {
        *(.data .data.*)
    }
    /* Inside the file: see the comment at the top. The section itself is NOBITS, but .rela.dyn comes after it, so
     * the flat Image (`lucina-boot loader-image`, which refuses memory beyond the file) carries it as zeros. The
     * entry code zeroes BSS on every start, including a restart from a copy, whose BSS holds the old copy's values. */
    .data.zero : ALIGN(16) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
        __stack_bottom = .;
        . += 0x10000;
        __stack_top = .;
    }
    .rela.dyn : ALIGN(8) {
        __rela_start = .;
        *(.rela .rela.*)
        __rela_end = .;
    }
    . = ALIGN(8);
    __file_end = .;
    . = ALIGN(4096);
    __image_end = .;

    /DISCARD/ : {
        *(.comment)
        *(.note .note.*)
        *(.eh_frame .eh_frame_hdr)
        *(.interp)
    }
}
