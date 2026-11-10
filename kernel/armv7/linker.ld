/*
 * LucinaOS ARMv7 kernel layout (3G/1G split, like Linux's PAGE_OFFSET = 0xC0000000).
 *
 *   0x00000000..0xC0000000  user space (Phase 1); at boot also an identity map of the image
 *   0xC0000000..0xE0000000  linear map of DRAM (512 MiB from the DRAM base); the image runs at 0xC0008000
 *   0xE0000000..0xF0000000  vmalloc / ioremap window (Phase 1; Phase 0 maps one 4 KiB test page there)
 *   0xF0000000..0xF4000000  fixed I/O window: VA = 0xF0000000 + PA for the i.MX6 peripherals below 64 MiB
 *
 * The image is position independent until the MMU is on: it is loaded at DRAM + 0x8000 by kexec (zImage
 * rules) or at DRAM + 0x10000 by QEMU, and boot.S moves it to DRAM + 0x8000 first.
 */
OUTPUT_ARCH(arm)
ENTRY(_start)

KERNEL_VBASE = 0xC0000000;
TEXT_OFFSET = 0x8000;

SECTIONS
{
    . = KERNEL_VBASE + TEXT_OFFSET;
    __image_start = .;

    .text : {
        KEEP(*(.text.head))
        *(.text.vectors)
        *(.text .text.*)
    }

    .rodata : ALIGN(8) {
        *(.rodata .rodata.*)
    }

    .ARM.extab : { *(.ARM.extab* .gnu.linkonce.armextab.*) }
    .ARM.exidx : { *(.ARM.exidx* .gnu.linkonce.armexidx.*) }

    .data : ALIGN(8) {
        *(.data .data.*)
    }
    . = ALIGN(8);
    __image_end = .;

    .bss (NOLOAD) : ALIGN(16384) {
        __bss_start = .;
        *(.bss.pagetable)
        *(.bss .bss.* COMMON)
        . = ALIGN(8);
        __bss_end = .;
    }

    .stack (NOLOAD) : ALIGN(4096) {
        __exc_stack_bottom = .;
        . += 16384;             /* IRQ, ABT, UND and FIQ: 4 KiB each */
        __exc_stack_top = .;
        __stack_bottom = .;
        . += 65536;
        __stack_top = .;
    }
    __kernel_end = .;

    /DISCARD/ : {
        *(.comment)
        *(.note .note.*)
    }
}

__image_size = __image_end - __image_start;
__bss_size = __bss_end - __bss_start;
