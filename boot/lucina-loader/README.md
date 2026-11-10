# LucinaLoader

LucinaLoader is the UEFI boot menu that replaces GRUB in LucinaOS on the Samsung Galaxy Book Go (Snapdragon 7c Gen 2,
SC7180, arm64 UEFI, Secure Boot off). It does what lucinaos' standalone GRUB and `boards/gbgo/grub.cfg` did, in Rust,
and boots the same Linux:

```
UEFI firmware -> EFI/BOOT/BOOTAA64.EFI = LucinaLoader -> menu (lucinaos/boot.toml)
    -> LoadImage/StartImage of the EFI-stub kernel (lucinaos/vmlinuz)
       + load options "BOOT_IMAGE=/lucinaos/vmlinuz <cmdline>"
       + device tree file as the DTB configuration table
       + initrd(s) through LoadFile2 on Linux's initrd device path
    -> the kernel's EFI stub exits boot services
```

The names:

- LucinaBoot (`boot/lucinaboot`) is the surya ABL chainloader.
- LucinaUEFI (`boot/lucina-uefi`) loads the Asterinas kernel itself: memory map, MMU off, EL2 drop.
- LucinaLoader starts kernels that have an EFI stub, through the firmware. It shares LucinaUEFI's way of calling the
  firmware: `src/efi.rs` started as a copy of LucinaUEFI's and was extended. LucinaUEFI itself is unchanged.

## What it does (each step is matched against the GRUB setup)

1. **Finds the menu.** It looks for `/lucinaos/boot.toml` on the FAT volume it was started from, then on every other
   one. GRUB's built-in `search --file /lucinaos/grub.cfg` took the first partition in the firmware's order.
   LucinaLoader prefers its own volume, so the installed laptop with the stick still plugged in boots its own copy.
2. **Reads the menu file.** That is `boot.toml`, plus the files it `include`s if they exist. The QEMU install test's
   `test.toml` takes the place of GRUB's `test.cfg`. It also reads the GRUB environment block named by `env`
   (`grubenv`, for the A/B update below).
   - The format is in `kernel/lucina-boot/src/menu.rs`.
   - It is a small TOML subset: top-level keys, `[vars]`, `[[when]]` and `[[entry]]`.
   - An entry's `if = "<file>"` and the `[[when]]` tables are grub.cfg's `if [ -f ... ]`.
   - GRUB's scripted parts have declarative equivalents:
     - `save = ["name=value"]` is GRUB's `save_env`. It rewrites the 1 KiB `grubenv` block in place and works
       at the menu and per entry.
     - `[vars.<group>]` tables, chosen with `with = "$lucina_step"`, are grub.cfg's `lucina_variant` function.
     - `if_env` conditions on variables and `unless` on a `[[when]]` cover the remaining `if` tests.
     - `action = "message"` covers entries that only print and wait.
     - A `devicetree` or `initrd` path that expands to nothing is left out.
     - Variables inside variables are expanded, as GRUB's `set` does.

     That is enough for lucinaos' 5-variant automatic diagnosis (`boards/gbgo/boot.toml`).
3. **Shows the menu on GOP** with the LucinaOS theme from `theme.txt`:
   - near-black `#070b18` background;
   - the white title "LucinaOS" and the accent `#7f8fe0` subtitle;
   - the selected entry on a navy `#2b3670` bar with a 4 px gold edge;
   - the countdown line "Starting in N s. Arrow keys and Enter to choose." over a gold progress bar, which fills one
     second at a time.

   The positions follow GRUB 2.06's own render of that theme: the theme's percentages plus pixel offsets measured at
   1024x768. The fonts are DejaVu Sans Bold 48, Regular 20, Bold 20 and Regular 16. `build.rs` rasterizes them from the
   same TTF files lucinaos' `lib/theme.sh` used, with anti-aliasing; GRUB's PF2 fonts were 1-bit.

   It first sets the first GOP mode from `modes` the firmware offers (GRUB's `gfxmode`), else keeps the current mode.
   Linux keeps that mode (GRUB's `gfxpayload=keep`).

   Without GOP (QEMU's serial tests) it shows a plain text menu on the console.
4. **Keys** work as in GRUB 2.06's menu:
   - Up and Down (or Ctrl-P and Ctrl-N) move without wrapping;
   - Home and Page Up go to the first entry, End and Page Down to the last;
   - Enter or Right starts the entry;
   - any key stops the countdown.

   A timeout of 0 boots the default without showing the menu. Keys still pressed from the firmware's boot menu are
   discarded at start.
5. **Starts Linux.**
   - **Kernel:** it reads the kernel and refuses a gzip image; Debian's arm64 `vmlinuz` is an uncompressed PE Image.
     It then calls `LoadImage` from memory.
   - **Command line:** it sets the load options to GRUB's exact command line. That is `BOOT_IMAGE=<path>` followed by
     the arguments, words separated by one space; empty variables leave no stray spaces.
   - **Device tree:** it installs the device tree as the `EFI_DTB_TABLE` configuration table. As GRUB's `devicetree`
     did, it first validates the tree (`lucina_fdt`), then copies it with 0x400 bytes of free space after it.
   - **Initrd:** it concatenates the initrd files, each padded to 4 bytes as GRUB did. It publishes them through
     `LoadFile2` on the `LINUX_EFI_INITRD_MEDIA_GUID` vendor media device path.
   - **Start:** it calls `StartImage`.

   The EFI stub then adds `/chosen` itself, with bootargs, the UEFI memory map and kaslr-seed (libstub
   `update_fdt`). GRUB 2.06 put the initrd in `/chosen` `linux,initrd-*`; the stub's LoadFile2 path replaces that.
   It logs `EFI stub: Loaded initrd from LINUX_EFI_INITRD_MEDIA_GUID device path`.

   If the kernel returns or a step fails, LucinaLoader puts the old DTB table back, removes the initrd protocol,
   unloads the image, shows the reason and returns to the menu.
6. **Firmware setup** works like GRUB's `fwsetup`. If `OsIndicationsSupported` has `BOOT_TO_FW_UI`, it sets that bit
   in `OsIndications` and restarts cold.

Nothing is written to the disk or to NVRAM, except `OsIndications` for firmware setup.

`unsafe` is confined to `src/efi.rs`. The decisions live in `kernel/lucina-boot/src/menu.rs`, which is `no_std`,
allocation-free and `unsafe`-free. Its host tests cover the gbgo menu file, entry visibility, command lines, keys,
the countdown and the theme layout measured on GRUB's render.

## Build

```
cd boot/lucina-loader && cargo build --release   # -> target/lucina-loader/aarch64-unknown-uefi/release/lucina-loader.efi
cargo test -p lucina-boot menu                  # the decisions, on the host
```

`build.rs` needs DejaVu Sans: `fonts-dejavu-core`, or set `LUCINA_LOADER_FONTS=<dir>`. The fonts are rasterized at
build time by `fontdue`, a build dependency only; the EFI binary contains just the coverage maps. The binary is about
175 KB; the GRUB standalone image was 3.4 MB.

lucinaos builds the Book Go stick with it: `boards/gbgo/build.sh --loader lucina` (`LUCINA_LOADER_SRC` points at
this directory). GRUB stays the build's default until LucinaOS has booted on the laptop.

## Not done yet

- **The A/B update slots.** lucinaos' gbgo-ota work keeps them in GRUB's environment block (`lucinaos/grubenv`:
  `lucina_slot`, `lucina_trial`, `lucina_tried`; slots `lucinaos/a|b/`).
  - LucinaLoader reads and rewrites that block as GRUB does.
  - Still missing: the trial step's read-back after the write; `probe --fs-uuid` (the FAT serial); and a fallback when
    a slot has no kernel. These wait for that design to be committed.
- **The laptop itself.** Untested so far:
  - the GOP modes the Book Go's panel offers;
  - the keyboard through `ConIn`;
  - `OsIndicationsSupported`;
  - LoadImage of Debian's kernel on Samsung's firmware.
