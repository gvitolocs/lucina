# LucinaBoot

The stage ABL starts as the "kernel" of a surya boot image (decision D-009, `docs/research/boot-chain.md` section 13).
It chainloads the LucinaOS kernel from RAM the loader already filled: no storage, USB, display or clock driver.

```
ABL (signed, untouched) -> boot.img v0: kernel = gzip(LucinaBoot Image) + ABL stub DTB, ramdisk = payload package
                        -> LucinaBoot (EL1, MMU off, x0 = ABL's DTB) -> kernel (x0 = patched DTB)
```

## What it does

1. `entry.S`: arm64 Image header (text_offset and image_size are filled in by `lucina-boot loader-image`), masks
   interrupts, cleans and invalidates its own image to PoC (lines ABL left dirty), applies its own
   `R_AARCH64_RELATIVE` relocations (it is linked at 0 and position independent), zeroes BSS, sets a 64 KiB stack.
   BSS and stack are inside the file, so nothing beyond the bytes ABL loaded is written before the memory plan.
2. Prints `CurrentEL` and the device tree on the PL011 named by `/chosen/stdout-path` (QEMU), and paints a status
   band on the `/chosen` simple-framebuffer (on surya the splash at 0x9c000000, named in the ABL stub tree): alive,
   EL colour, five stage cells, a verdict cell, and on error the code in binary (`kernel/lucina-boot/src/band.rs`;
   codes in `src/main.rs`: 1 device tree, 2 bad tree, 3 no payload, 4 package, 5 kernel image, 6 memory plan,
   7 too many or overflowing ranges, 8 panic, 9 packaged tree does not reserve what ABL's tree reserves,
   10 packaged tree has more than one memory node).
3. Reads the payload package (`kernel/lucina-boot/src/pkg.rs`) from `/chosen` `linux,initrd-start/end`.
4. Picks the device tree for the panel ABL names in `msm_drm.dsi_display0=` (huaxing / tianma, falling back to
   huaxing, then `default`, then the only one), else uses ABL's own.
5. Plans RAM (`kernel/lucina-boot/src/plan.rs`): the kernel goes to the lowest 2 MiB-aligned RAM base plus its
   `text_offset`; if LucinaBoot itself is in the way it copies itself elsewhere and starts again; if the package is
   in the way it is moved; the device tree gets 2 MiB of free RAM. Reserved memory of both trees is avoided, and a
   packaged tree must reserve everything ABL's tree reserves (the kernel only sees the packaged tree).
6. Writes the device tree with `/chosen` (`bootargs`, `linux,initrd-start/end`) and `/memory` set
   (`lucina_fdt::patch`), copies the kernel, cleans the data cache to PoC, invalidates the I-cache and jumps with
   `x0` = device tree, `x1`-`x3` = 0.
7. On any error it prints the reason, waits 3 s and resets through PSCI (`/psci` `method`). After a RAM-only
   `fastboot boot` the phone then starts Android from its own `boot` partition; were LucinaBoot ever written to
   `boot` (not allowed, D-007) it would come back to LucinaBoot. QEMU with `--no-reboot` exits.

`unsafe` is confined to `src/arch.rs` (system registers, cache maintenance, raw memory, the jump),
`src/console.rs` (PL011 registers) and `src/screen.rs` (framebuffer pixels). The decisions are in
`kernel/lucina-boot` and `kernel/lucina-fdt`, which are `no_std`, `unsafe`-free and tested on the host.

## Build and test

```
cd boot/lucinaboot && cargo build --release      # target aarch64-unknown-none-softfloat (.cargo/config.toml)
scripts/qemu-lucinaboot.sh bare|fb|chain|move|panel  # QEMU virt, pinned image; suites in tests/parity/
scripts/pack-surya.sh --check [--no-payload]     # host-only surya boot.img, checked with AOSP unpack_bootimg
```

Soft-float and `+strict-align` matter on the phone: with the MMU off every data access is Device memory, where
unaligned accesses fault, and nothing may depend on how ABL left CPACR_EL1.

## Not done

- Text output on the phone: only the band. surya's debug UART is GENI, not PL011, and needs the phone opened; a
  ramoops record (`0x9d800000`) that Android could show after the reset is not written yet (T-KER-06).
- The band was checked in QEMU (`qemu-lucinaboot-fb`), not on the panel: whether ABL keeps scanning out the splash
  after it jumps, and in which format, is what experiment E1 (T-DEV-01) shows.
- The ABL stub device tree's reserved map comes from the research notes; compare it with the tree ABL really
  passes (T-DEV-00) before the first run on the phone.
- Copy speed with the MMU off is unknown on the phone: every access is Device memory, and the kernel (5 MB) and a
  moved package (tens of MB) are copied 8 bytes at a time. If E1 shows it is slow, map RAM write-back for the
  copies; the band shows which stage it is in meanwhile.
- Nothing has run on the phone.
