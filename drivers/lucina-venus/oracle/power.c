/* Linux's own runtime suspend and resume of the block's processor (hfi_venus.c: venus_suspend -> venus_suspend_3xx, with
 * venus_prepare_power_collapse and venus_power_off; venus_resume -> venus_power_on), run after main.c's core init and session against
 * the same model, with the registers those functions read added:
 *   WRAPPER_CPU_STATUS            WFI (the processor waits for work)
 *   CPU_CS_SCIACMDARG0            1, the idle message bit, and PC_READY once SYS_PC_PREP has been sent
 *   WRAPPER_CPU_AXI_HALT_STATUS   idle
 * Prints main.c's lines, then "power", then every register write, command and secure-world call of the suspend and the resume.
 * tests/power.rs runs Core::suspend and Core::resume against the same registers and compares what follows "power". */
#define main oracle_session_main
#define oracle_readl oracle_readl_session
#define oracle_writel oracle_writel_session
#include "main.c"
#undef main
#undef oracle_readl
#undef oracle_writel

static int suspending, pc_prep_sent;

u32 oracle_readl(const volatile void *a) {
    size_t off = (const u8 *)a - regs;
    if (suspending) {
        if (off == WRAPPER_BASE + WRAPPER_CPU_STATUS) return WRAPPER_CPU_STATUS_WFI;
        if (off == CPU_CS_BASE + CPU_CS_SCIACMDARG0)
            return 1 | CPU_CS_SCIACMDARG0_INIT_IDLE_MSG_MASK | (pc_prep_sent ? CPU_CS_SCIACMDARG0_PC_READY : 0);
        if (off == WRAPPER_BASE + WRAPPER_CPU_AXI_HALT_STATUS) return WRAPPER_CPU_AXI_HALT_STATUS_IDLE;
    }
    return oracle_readl_session(a);
}

void oracle_writel(u32 value, volatile void *a) {
    size_t off = (const u8 *)a - regs;
    oracle_writel_session(value, a);
    if (suspending && off == CPU_IC_BASE + CPU_IC_SOFTINT) pc_prep_sent = 1;
}

int main(void) {
    int ret = oracle_session_main();
    if (ret) return ret;
    printf("power\n");
    suspending = 1;
    ret = core.ops->suspend(&core);
    printf("suspend %d\n", ret);
    suspending = 0;
    ret = core.ops->resume(&core);
    printf("resume %d\n", ret);
    return 0;
}
