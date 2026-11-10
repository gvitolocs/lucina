# LucinaUEFI

A UEFI application that boots the LucinaOS kernel (Asterinas v0.19.0 + `kernel/patches/`) on AArch64 laptops with
UEFI firmware. First board: the Samsung Galaxy Book Go (Snapdragon 7c Gen 2, SC7180), started from a USB stick
through the F10 boot menu (decision D-019, `docs/research/gbgo-port.md`).

```
UEFI firmware (Samsung/Qualcomm, Secure Boot off) -> EFI/BOOT/BOOTAA64.EFI = LucinaUEFI (EL1 or EL2, MMU on)
    -> kernel (EL1, MMU off, x0 = device tree)
```

## What it does

1. Prints `CurrentEL`, the firmware vendor and which configuration tables exist (ACPI, device tree, SMBIOS).
2. Reads `\LUCINA\BOOT.CFG` from the volume it was started from (`lucina_boot::uefi::Config`):
   ```
   kernel  = \LUCINA\KERNEL-A0.IMG 0xa0000000   # path and the OSTD_ARM_DRAM_BASE it was built with; up to 4, in order
   initrd  = \LUCINA\INITRAMFS.GZ                # optional
   dtb     = \LUCINA\GBGO.DTB                    # optional: else the firmware's device tree table
   cmdline = console=tty0 loglevel=info -- sh -l
   rng     = off                                 # optional: do not ask the firmware for a random seed
   pause   = 15                                  # seconds before ExitBootServices (time to photograph the screen)
   verbose = 1                                   # also print the whole memory map on screen
   log     = \LUCINA\LOG.TXT                     # written before ExitBootServices (default \LUCINA\LOG.TXT)
   ```
3. Reads the GOP mode and turns it into a `/chosen/framebuffer@<addr>` `simple-framebuffer` (x8r8g8b8; padded lines
   are described as a wider screen, since the kernel's framebuffer console has no stride; an RGBX firmware swaps red
   and blue; BLT-only GOP means no kernel log on screen).
4. Loads the first kernel whose window is free RAM. Asterinas only runs at its link address,
   `OSTD_ARM_DRAM_BASE + text_offset` (D-013), so each candidate is a build for one base; the window is allocated
   at that exact address in whole 2 MiB blocks (`AllocatePages(AllocateAddress)` succeeds only on free RAM). If no
   candidate fits, it prints a base that would (`lucina_boot::uefi::suggest_base`).
5. Loads the initramfs and the device tree, below 4 GiB when it can.
6. Builds the device tree with `lucina_fdt::patch` (allocation-free): `/memory` = the RAM of the UEFI memory map that
   the kernel may use (conventional, loader and boot-services memory that is write-back cacheable; runtime services,
   ACPI, reserved, MMIO, unusable and specific-purpose memory are left out, so the kernel never maps them), extra
   memory nodes emptied, `/chosen` `bootargs`, `linux,initrd-start/end` and the framebuffer. The tree's own
   `/reserved-memory` (the Qualcomm carve-outs) stays as it is.
   Before that it asks the firmware's `EFI_RNG_PROTOCOL` for 32 bytes for `/chosen/rng-seed`, with a marker on the disk around the
   call, so that a firmware that hangs in it costs one restart (`lucina_boot::rng_marker`, `docs/research/juno-random.md`).
7. Writes everything it printed to the log file, counts down `pause`, then exits boot services. From the first
   ExitBootServices call on it only reads the memory map again and rebuilds the tree from it (no allocation, no
   output) until the call succeeds.
8. Paints the status band (`lucina_boot::band`: alive, EL colour, 5 stages, verdict) on the framebuffer, cleans the
   kernel, initramfs, tree and itself to the point of coherency, turns the MMU and data cache off and jumps with
   `x0` = tree. Entered at EL2, it first configures EL1 (HCR_EL2.RW, no traps, no stage 2, timer access,
   `CNTVOFF_EL2 = 0`, GICv3 system registers, `VPIDR`/`VMPIDR`) and `eret`s into the kernel at EL1h with interrupts
   masked.

Errors before ExitBootServices are printed, logged and shown on the band (codes: 1 config, 2 kernel, 3 initramfs,
4 device tree, 5 memory map, 6 ExitBootServices, 7 entered at EL3, 8 panic); after 15 s the loader returns to the
firmware's boot menu. After ExitBootServices only the band can show an error, and the loader stops.

`unsafe` is confined to `src/efi.rs` (firmware calls), `src/arch.rs` (system registers, caches, the trampoline)
and `src/screen.rs` (framebuffer pixels). The UEFI interface is hand-written for the calls used (no crates.io
dependency); the decisions live in `kernel/lucina-boot/src/uefi.rs` and are tested on the host.

## Build and test

```
cd boot/lucina-uefi && cargo build --release       # target aarch64-unknown-uefi -> target/lucina-uefi/.../lucina-uefi.efi
scripts/qemu-uefi.sh el1|el2                       # QEMU virt + edk2 from the pinned image, USB stick image (tests/parity/qemu-uefi-*.toml)
scripts/gbgo-stick.sh [--smoke]                    # the Galaxy Book Go stick image (out/gbgo/), never written to a device
```

## Not done

- Nothing has run on the laptop. The firmware's exception level, GOP mode and memory map there are what the first
  boot prints (procedure in `docs/research/gbgo-port.md`).
- No runtime services: `SetVirtualAddressMap` is not called and the kernel cannot use UEFI variables or reset.
- No Secure Boot signing.
