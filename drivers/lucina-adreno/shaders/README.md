# Shaders of `lucina_adreno::quad`

The words in `quad.rs` are machine code for the A618, pasted as constants. Nothing compiles shaders on Juno.

- `fs_rgba.asm`: `FRAGMENT_SHADER`, from Scarlet's A618 pack (Mesa IR3 at `3f1b217b`). Re-assembling it here gives the same words,
  which is the check that the assembler below is set up right.
- `fs_yuv.asm`: `YUV_FRAGMENT_SHADER`, written for this pipeline's registers: the texture arrives in r0.z.. (sampler pre-dispatch,
  `SP_PS_INITIAL_TEX_LOAD_CMD`), the colour leaves from r1.z.. (`SP_PS_OUTPUT_REG`), constants c0..c7 (`SP_PS_CONST_CONFIG`).

To assemble, on buildhost with Docker (Mesa is MIT; nothing of it is copied into this crate):

```sh
curl -fsSLO https://archive.mesa3d.org/mesa-25.2.6.tar.xz   # sha256 361c97e8afa5fe20141c5362c5b489040751e12861c186a16c621a2fb182fc42
tar xf mesa-25.2.6.tar.xz && mkdir tool && cp asm.c *.asm tool/
docker run --rm -v $PWD:/src debian:trixie bash /src/build-mesa.sh     # Mesa's freedreno tools, about 10 minutes
# then compile asm.c against that build: the include and link lines of build/src/freedreno/ir3/ir3_disasm in build.ninja
docker run --rm -v $PWD:/src debian:trixie /src/tool/ir3-asm /src/tool/fs_yuv.asm
```

`ir3-asm` prints the words in the order `quad.rs` keeps them and the disassembly. The assembler does not schedule: the delay slots
in `fs_yuv.asm` are counted by hand against Mesa's `ir3_delayslots`.
