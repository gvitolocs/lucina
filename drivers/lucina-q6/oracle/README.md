# Oracle for lucina-q6

Linux's own `q6afe.c`, `q6adm.c`, `q6asm.c`, `q6core.c` and `q6dsp-common.c`, unchanged, compile here against `fake.h` instead of the kernel.
`main.c` calls them as `sc7180.c`, `q6afe-dai.c`, `q6routing.c`, `q6asm-dai.c` and ALSA's DPCM do for one playback on tertiary MI2S, answers as
the callbacks expect, and prints `<case> <hex>` for every packet `apr_send_pkt` is given. `tests/oracle.rs` makes the same packets with
`Playback` and compares them, in that order.

So the fields and sizes are what Linux's code puts there, not what someone read from it. Three things are not run: `q6asm-dai.c`, `q6routing.c`
and `sc7180.c` need ALSA, so the arguments and the order in `main.c` are read from them (`docs/research/juno-audio.md` section 11), and whether
the DSP accepts the packets only the laptop can say. `q6core.c` sends a packet from its stack with its ports and token unset; those are zeroed
here and are zero in ours.

Pin: Linux 7.1 in buildhost `~/Projects/Lucina/lucinaos-kernel`. GPL-2.0, like the crate. `fixtures/oracle.txt` is the output, in git.

```sh
cd drivers/lucina-q6/oracle && bash build.sh
```

## `va/`: the microphone's VA macro and pins

`va/build.sh` cuts Linux's VA macro functions out of `lpass-va-macro.c` (the register names, `struct va_macro`, `va_defaults`, the register
tables and the functions DAPM and ALSA call), and `lpi_gpio_set_mux` with the SM8250 pin table out of the LPI pin driver, unchanged, into one
file. `fake-va.h` stands in for the kernel, and for regmap with the code of `regmap.c`, `regcache.c` and `regcache-flat.c` that an MMIO map with
a flat cache runs, cut to the paths it takes. `main-va.c` runs two captures from DMIC0 into decimator 0 in DPCM's order and prints every
access that reaches the hardware. `tests/va.rs` runs `va.rs` on registers that start the same and compares each line.

```sh
cd drivers/lucina-q6/oracle/va && bash build.sh
```
