// SPDX-License-Identifier: GPL-2.0-only
/* Assembles ir3 text for the A618 and prints the words, for lucina-adreno. */
#include <stdio.h>
#include <string.h>
#include "ir3/ir3_assembler.h"
#include "ir3/ir3_shader.h"
#include "ir3/ir3_compiler.h"
#include "common/freedreno_dev_info.h"
#include "ir3-isa.h"
int main(int argc, char **argv) {
   struct fd_dev_id id = {.gpu_id = 618};
   struct ir3_compiler *c = ir3_compiler_create(NULL, &id, fd_dev_info_raw(&id), &(struct ir3_compiler_options){});
   FILE *in = fopen(argv[1], "r");
   struct ir3_kernel_info info = {};
   struct ir3_shader *s = ir3_parse_asm(c, &info, in);
   if (!s) { fprintf(stderr, "parse failed\n"); return 1; }
   struct ir3_shader_variant *v = s->variants;
   unsigned n = v->info.size / 4;
   for (unsigned i = 0; i < n; i++) printf("0x%04x_%04x,%s", v->bin[i] >> 16, v->bin[i] & 0xffff, (i % 2) ? "\n" : " ");
   ir3_isa_disasm(v->bin, v->info.size, stderr, &(struct isa_decode_options){.gpu_id = 618, .show_errors = true});
   return 0;
}
