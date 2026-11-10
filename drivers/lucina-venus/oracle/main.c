/* Drives Linux's own hfi_venus.c, hfi_cmds.c and hfi_msgs.c as core.c does for the SC7180 (sc7180_res: HFI 4XX, AR50, no reg_tbl, no
 * ubwc_conf, no min_fw), against a model of the block's registers and of its firmware. Prints, in order:
 *   w <offset> <value>        every register write, offsets from the block's base; addresses of the shared memory as table+N, sfr+N
 *   table <words>             the queue table and the three queue headers after venus_hfi_create
 *   cmd <words>               every command packet the firmware model finds in the command queue
 *   core error <n> version <major>.<minor>-<rev>   what hfi_msgs.c made of the firmware model's answers
 *   session, then bufreq, event, done, flushed: one H.264 session as vdec.c and helpers.c run it, and what hfi_msgs.c handed back
 * tests/oracle.rs runs the same steps with the crate and compares. */
#include "core.h"
#include "hfi.h"
#include "hfi_cmds.h"
#include "hfi_msgs.h"
#include "hfi_venus.h"
#include "hfi_venus_io.h"
#include "hfi_plat_bufs.h"

static u8 regs[0x100000];
#define DMA_BASE 0xd0000000u
static u8 dma[8 << 20] __attribute__((aligned(4096)));
static size_t dma_used;
static dma_addr_t table_da, sfr_da;
static int pending;

void *oracle_dma_alloc(size_t size, dma_addr_t *da) {
    size = ALIGN(size, 4096);
    if (dma_used + size > sizeof(dma)) return NULL;
    void *p = dma + dma_used;
    *da = DMA_BASE + dma_used;
    dma_used += size;
    memset(p, 0, size);
    if (!table_da) table_da = *da; else if (!sfr_da) sfr_da = *da;
    return p;
}

static void *kva(u32 da) { return dma + (da - DMA_BASE); }

static void print_value(u32 v) {
    if (table_da && v >= table_da && v < table_da + 0x300000) printf("table+%x", v - (u32)table_da);
    else if (sfr_da && v >= sfr_da && v < sfr_da + 0x1000) printf("sfr+%x", v - (u32)sfr_da);
    else printf("%x", v);
}

/* The queue of index i in Linux's layout: header in the table, ring at its start_addr. */
static u32 *qhdr(int i) { return (u32 *)kva(table_da) + 6 + 14 * i; }

static void put_message(const u32 *words) {
    u32 *h = qhdr(1);
    u32 *ring = kva(h[1]);
    u32 n = words[0] / 4, wr = h[13];
    for (u32 k = 0; k < n; k++) ring[(wr + k) % h[3]] = words[k];
    h[13] = (wr + n) % h[3];
    pending = 1;
}


/* The firmware's side of a decode session: the answers Linux waits for, a requirements table, the sequence-changed event after the
 * first input buffer, a picture for every linear output buffer once the input has been seen. */
static const u32 bufreq_table[][8] = {
    /* type, size, region_size, hold_count (count_min on 4xx), count_min (count_min_host), count_actual, contiguous, alignment */
    { HFI_BUFFER_INPUT, 1568768, 0, 2, 2, 4, 1, 4096 },
    { HFI_BUFFER_OUTPUT, 3248128, 0, 6, 6, 8, 1, 4096 },
    { HFI_BUFFER_OUTPUT2, 3133440, 0, 6, 6, 8, 1, 4096 },
    { HFI_BUFFER_INTERNAL_SCRATCH(HFI_VERSION_4XX), 0x300000, 0, 1, 1, 1, 1, 4096 },
    { HFI_BUFFER_INTERNAL_SCRATCH_1(HFI_VERSION_4XX), 0x80000, 0, 1, 1, 1, 1, 4096 },
    { HFI_BUFFER_INTERNAL_SCRATCH_2(HFI_VERSION_4XX), 0, 0, 0, 0, 0, 1, 4096 },
    { HFI_BUFFER_INTERNAL_PERSIST, 0x100000, 0, 1, 1, 1, 1, 4096 },
    { HFI_BUFFER_INTERNAL_PERSIST_1, 0x40000, 0, 1, 1, 1, 1, 4096 },
};
/* The same counts after the resolution change to 1280 x 720, with that size's UBWC and NV12 buffers. */
static const u32 bufreq_table_720[][8] = {
    { HFI_BUFFER_INPUT, 1568768, 0, 2, 2, 4, 1, 4096 },
    { HFI_BUFFER_OUTPUT, 1507328, 0, 6, 6, 8, 1, 4096 },
    { HFI_BUFFER_OUTPUT2, 1413120, 0, 6, 6, 8, 1, 4096 },
    { HFI_BUFFER_INTERNAL_SCRATCH(HFI_VERSION_4XX), 0x300000, 0, 1, 1, 1, 1, 4096 },
    { HFI_BUFFER_INTERNAL_SCRATCH_1(HFI_VERSION_4XX), 0x80000, 0, 1, 1, 1, 1, 4096 },
    { HFI_BUFFER_INTERNAL_SCRATCH_2(HFI_VERSION_4XX), 0, 0, 0, 0, 0, 1, 4096 },
    { HFI_BUFFER_INTERNAL_PERSIST, 0x100000, 0, 1, 1, 1, 1, 4096 },
    { HFI_BUFFER_INTERNAL_PERSIST_1, 0x40000, 0, 1, 1, 1, 1, 4096 },
};
static int inputs_seen, event_sent, second_event_sent, frame_no;

static void session_done(u32 type, u32 sid) {
    u32 m[8] = { 32, type, sid, 0 };
    put_message(m);
}

static void session_firmware(const u32 *pkt) {
    u32 sid = pkt[2];
    switch (pkt[1]) {
    case HFI_CMD_SYS_SESSION_INIT: session_done(HFI_MSG_SYS_SESSION_INIT, sid); break;
    case HFI_CMD_SYS_SESSION_END: session_done(HFI_MSG_SYS_SESSION_END, sid); break;
    case HFI_CMD_SESSION_LOAD_RESOURCES: session_done(HFI_MSG_SESSION_LOAD_RESOURCES, sid); break;
    case HFI_CMD_SESSION_START: session_done(HFI_MSG_SESSION_START, sid); break;
    case HFI_CMD_SESSION_STOP: session_done(HFI_MSG_SESSION_STOP, sid); break;
    case HFI_CMD_SESSION_RELEASE_RESOURCES: session_done(HFI_MSG_SESSION_RELEASE_RESOURCES, sid); break;
    case HFI_CMD_SESSION_RELEASE_BUFFERS: session_done(HFI_MSG_SESSION_RELEASE_BUFFERS, sid); break;
    case HFI_CMD_SESSION_FLUSH: {
        u32 m[] = { 20, HFI_MSG_SESSION_FLUSH, sid, 0, pkt[3] };
        put_message(m);
        break;
    }
    case HFI_CMD_SESSION_GET_PROPERTY: {
        u32 m[5 + 8 * 8] = { 20 + 32 * 8, HFI_MSG_SESSION_PROPERTY_INFO, sid, 1, HFI_PROPERTY_CONFIG_BUFFER_REQUIREMENTS };
        memcpy(&m[5], second_event_sent ? bufreq_table_720 : bufreq_table, sizeof(bufreq_table));
        put_message(m);
        break;
    }
    case HFI_CMD_SESSION_EMPTY_BUFFER: {
        /* EBD: error, offset, filled_len, input_tag, packet_buffer, extradata. */
        u32 m[] = { 36, HFI_MSG_SESSION_EMPTY_BUFFER, sid, 0, pkt[8], pkt[10], pkt[11], pkt[12], 0 };
        put_message(m);
        inputs_seen++;
        if (!event_sent) {
            event_sent = 1;
            u32 e[] = { 0, HFI_MSG_EVENT_NOTIFY, sid, HFI_EVENT_SESSION_SEQUENCE_CHANGED, HFI_EVENT_DATA_SEQUENCE_CHANGED_SUFFICIENT_BUF_RESOURCES, 5,
                HFI_PROPERTY_PARAM_FRAME_SIZE, HFI_BUFFER_OUTPUT2, 1920, 1080,
                HFI_PROPERTY_PARAM_VDEC_PIXEL_BITDEPTH, HFI_BUFFER_OUTPUT2, 0,
                HFI_PROPERTY_PARAM_VDEC_PIC_STRUCT, 1,
                HFI_PROPERTY_PARAM_VDEC_DPB_COUNTS, 5, 4, 4, 2, 6,
                HFI_INDEX_EXTRADATA_INPUT_CROP, 28, 1, 1, 0, 0, 1920, 1080 };
            e[0] = sizeof(e);
            put_message(e);
        } else if (!second_event_sent && inputs_seen == 3) {
            /* The resolution change: a second sequence-changed event, 1280 x 720, after the seek's input buffer. */
            second_event_sent = 1;
            u32 e[] = { 0, HFI_MSG_EVENT_NOTIFY, sid, HFI_EVENT_SESSION_SEQUENCE_CHANGED, HFI_EVENT_DATA_SEQUENCE_CHANGED_SUFFICIENT_BUF_RESOURCES, 5,
                HFI_PROPERTY_PARAM_FRAME_SIZE, HFI_BUFFER_OUTPUT2, 1280, 720,
                HFI_PROPERTY_PARAM_VDEC_PIXEL_BITDEPTH, HFI_BUFFER_OUTPUT2, 0,
                HFI_PROPERTY_PARAM_VDEC_PIC_STRUCT, 1,
                HFI_PROPERTY_PARAM_VDEC_DPB_COUNTS, 5, 4, 4, 2, 6,
                HFI_INDEX_EXTRADATA_INPUT_CROP, 28, 1, 1, 0, 0, 1280, 720 };
            e[0] = sizeof(e);
            put_message(e);
        }
        break;
    }
    case HFI_CMD_SESSION_FILL_BUFFER: {
        if (pkt[3] != 1) break; /* DPB buffers stay with the firmware here */
        frame_no++;
        u32 m[25] = { 100, HFI_MSG_SESSION_FILL_BUFFER, sid, 1, 0, 0, 0, 40000 * frame_no, 0, 0, 0, 0,
                      pkt[5], 3133440, 0, 1920, 1088, 0, 0, 0, 0, pkt[7], HFI_PICTURE_P, pkt[8], 0 };
        put_message(m);
        break;
    }
    default:
        break;
    }
}

/* The firmware: every command it finds, printed, and what it answers. */
static void firmware(void) {
    u32 *h = qhdr(0);
    u32 *ring = kva(h[1]);
    while (h[12] != h[13]) {
        u32 rd = h[12], n = ring[rd] / 4;
        printf("cmd");
        u32 pkt[256];
        for (u32 k = 0; k < n; k++) { pkt[k] = ring[(rd + k) % h[3]]; printf(" %x", pkt[k]); }
        printf("\n");
        h[12] = (rd + n) % h[3];
        switch (pkt[1]) {
        case HFI_CMD_SYS_INIT: {
            u32 m[] = { 28, HFI_MSG_SYS_INIT, 0, 1, HFI_PROPERTY_PARAM_CODEC_SUPPORTED, 0x7012, 0x3002 };
            put_message(m);
            break;
        }
        case HFI_CMD_SYS_GET_PROPERTY: {
            u32 m[4 + 32] = { 16 + 128, HFI_MSG_SYS_PROPERTY_INFO, 1, HFI_PROPERTY_SYS_IMAGE_VERSION };
            strcpy((char *)&m[4], "14:VIDEO.VE.5.4-00032-PROD-1");
            put_message(m);
            break;
        }
        case HFI_CMD_SYS_PING: {
            u32 m[] = { 12, HFI_MSG_SYS_PING_ACK, pkt[2] };
            put_message(m);
            break;
        }
        default:
            session_firmware(pkt);
            break;
        }
    }
}

u32 oracle_readl(const volatile void *a) {
    size_t off = (const u8 *)a - regs;
    if (off == CPU_CS_BASE + CPU_CS_SCIACMDARG0) return 1;
    if (off == WRAPPER_BASE + WRAPPER_INTR_STATUS) return pending ? WRAPPER_INTR_STATUS_A2H_MASK : 0;
    u32 v;
    memcpy(&v, regs + off, 4);
    return v;
}

void oracle_writel(u32 value, volatile void *a) {
    size_t off = (const u8 *)a - regs;
    printf("w %zx ", off);
    print_value(value);
    printf("\n");
    memcpy(regs + off, &value, 4);
    if (off == CPU_IC_BASE + CPU_IC_SOFTINT) firmware();
    if (off == WRAPPER_BASE + WRAPPER_INTR_CLEAR) pending = 0;
}

int venus_set_hw_state(struct venus_core *core, bool resume) {
    printf("scm set_remote_state %d\n", resume ? 1 : 0);
    return 0;
}

void *qcom_smem_get(unsigned host, unsigned item, size_t *size) { return ERR_PTR(-ENODEV); }
struct v4l2_fh *file_to_v4l2_fh(struct file *f) { return NULL; }
int hfi_plat_bufreq_v6(struct hfi_plat_buffers_params *params, u32 session_type, u32 buftype, struct hfi_buffer_requirements *bufreq) {
    return -EINVAL;
}

static const struct venus_resources sc7180_res = {
    .hfi_version = HFI_VERSION_4XX,
    .vpu_version = VPU_VERSION_AR50,
    .vmem_id = VIDC_RESOURCE_NONE,
    .dma_mask = 0xe0000000 - 1,
    .cp_start = 0,
    .cp_size = 0x70800000,
    .cp_nonpixel_start = 0x1000000,
    .cp_nonpixel_size = 0x24800000,
};

static void interrupt(struct venus_core *core) {
    while (pending) {
        core->ops->isr(core);
        core->ops->isr_thread(core);
    }
}

static void buf_done(struct venus_inst *inst, unsigned int buf_type, u32 tag, u32 bytesused, u32 data_offset, u32 flags, u32 hfi_flags,
                     u64 timestamp_us) {
    printf("done %u tag %u bytes %u off %u flags %x hfi %x ts %llu\n", buf_type, tag, bytesused, data_offset, flags, hfi_flags,
           (unsigned long long)timestamp_us);
}

static void event_notify(struct venus_inst *inst, u32 event, struct hfi_event_data *d) {
    printf("event %u type %x w %u h %u depth %u pic %u count %u crop %u %u %u %u\n", event, d->event_type, d->width, d->height,
           d->bit_depth, d->pic_struct, d->buf_count, d->input_crop.left, d->input_crop.top, d->input_crop.width, d->input_crop.height);
}

static void flush_done(struct venus_inst *inst) { printf("flushed\n"); }

static const struct hfi_inst_ops inst_ops = { .buf_done = buf_done, .event_notify = event_notify, .flush_done = flush_done };

static struct venus_core core;
static struct venus_inst inst;

/* What a synchronous command waits for, and an asynchronous one gets back at the next interrupt. */
static void run(int ret) {
    if (ret) printf("error %d\n", ret);
    interrupt(&core);
}

static void set(u32 ptype, void *data) { run(core.ops->session_set_property(&inst, ptype, data)); }

static void bufreq(void) {
    run(core.ops->session_get_property(&inst, HFI_PROPERTY_CONFIG_BUFFER_REQUIREMENTS));
    for (int i = 0; i < HFI_BUFFER_TYPE_MAX && inst.hprop.bufreq[i].type; i++) {
        struct hfi_buffer_requirements *r = &inst.hprop.bufreq[i];
        printf("bufreq %u size %u min %u actual %u\n", r->type, r->size, hfi_bufreq_get_count_min(r, HFI_VERSION_4XX), r->count_actual);
    }
}

/* vdec_output_conf for 4xx with UBWC: the eight properties and the requirements between them. */
static void output_conf(u32 w, u32 h, u32 nv12_size, u32 ubwc_size) {
    struct hfi_video_work_mode wm = { (w / 16) * (ALIGN(h, 32) / 16) <= 3680 ? 1 : 2 };
    set(HFI_PROPERTY_PARAM_WORK_MODE, &wm);
    struct hfi_uncompressed_format_select f2 = { HFI_BUFFER_OUTPUT2, HFI_COLOR_FORMAT_NV12 };
    set(HFI_PROPERTY_PARAM_UNCOMPRESSED_FORMAT_SELECT, &f2);
    struct hfi_multi_stream m1 = { HFI_BUFFER_OUTPUT, 0 }, m2 = { HFI_BUFFER_OUTPUT2, 1 };
    set(HFI_PROPERTY_PARAM_VDEC_MULTI_STREAM, &m1);
    set(HFI_PROPERTY_PARAM_VDEC_MULTI_STREAM, &m2);
    struct hfi_uncompressed_format_select f1 = { HFI_BUFFER_OUTPUT, HFI_COLOR_FORMAT_NV12_UBWC };
    set(HFI_PROPERTY_PARAM_UNCOMPRESSED_FORMAT_SELECT, &f1);
    struct hfi_framesize fs = { HFI_BUFFER_OUTPUT2, w, ALIGN(h, 32) };
    set(HFI_PROPERTY_PARAM_FRAME_SIZE, &fs);
    bufreq();
    struct hfi_buffer_size_actual s2 = { HFI_BUFFER_OUTPUT2, nv12_size }, s1 = { HFI_BUFFER_OUTPUT, ubwc_size };
    set(HFI_PROPERTY_PARAM_BUFFER_SIZE_ACTUAL, &s2);
    set(HFI_PROPERTY_PARAM_BUFFER_SIZE_ACTUAL, &s1);
}

static void num_bufs(u32 inputs) {
    struct hfi_buffer_count_actual c[] = { { HFI_BUFFER_INPUT, inputs }, { HFI_BUFFER_OUTPUT, 32 }, { HFI_BUFFER_OUTPUT2, 32 } };
    for (int i = 0; i < 3; i++) set(HFI_PROPERTY_PARAM_BUFFER_COUNT_ACTUAL, &c[i]);
}

static const u32 internal[] = { HFI_BUFFER_INTERNAL_SCRATCH(HFI_VERSION_4XX), HFI_BUFFER_INTERNAL_SCRATCH_1(HFI_VERSION_4XX), HFI_BUFFER_INTERNAL_SCRATCH_2(HFI_VERSION_4XX),
                                HFI_BUFFER_INTERNAL_PERSIST, HFI_BUFFER_INTERNAL_PERSIST_1 };

/* Every buffer at an address from its type and index, so that both sides name them alike. */
static u32 address(u32 type, u32 i) { return 0x90000000u + type * 0x1000000u + i * 0x400000u; }

static void set_internal(u32 type) {
    bufreq();
    for (int i = 0; i < HFI_BUFFER_TYPE_MAX && inst.hprop.bufreq[i].type; i++) {
        struct hfi_buffer_requirements *r = &inst.hprop.bufreq[i];
        if (r->type != type || !r->size) continue;
        for (u32 k = 0; k < r->count_actual; k++) {
            struct hfi_buffer_desc bd = { .buffer_type = type, .buffer_size = r->size, .num_buffers = 1, .device_addr = address(type, k) };
            run(core.ops->session_set_buffers(&inst, &bd));
        }
    }
}

static void release_internal(u32 type, u32 size) {
    struct hfi_buffer_desc bd = { .buffer_type = type, .buffer_size = size, .num_buffers = 1, .device_addr = address(type, 0),
                                  .response_required = 1 };
    run(core.ops->session_unset_buffers(&inst, &bd));
}

static void etb(u32 tag, u32 flags, u32 len, u64 ts) {
    struct hfi_frame_data in = { .buffer_type = HFI_BUFFER_INPUT, .device_addr = flags ? 0xdeadb000 : address(HFI_BUFFER_INPUT, tag),
                                 .timestamp = ts, .flags = flags, .alloc_len = flags ? 0 : 1568768, .filled_len = len, .clnt_data = tag };
    run(core.ops->session_etb(&inst, &in));
}

static void ftb(u32 type, u32 tag, u32 size) {
    struct hfi_frame_data out = { .buffer_type = type, .device_addr = address(type, tag), .alloc_len = size, .clnt_data = tag };
    run(core.ops->session_ftb(&inst, &out));
}

static void session(void) {
    inst.core = &core;
    inst.session_type = VIDC_SESSION_TYPE_DEC;
    inst.ops = &inst_ops;
    init_completion(&inst.done);
    INIT_LIST_HEAD(&inst.list);
    list_add_tail(&inst.list, &core.instances);
    printf("session\n");
    /* vdec_session_init */
    run(core.ops->session_init(&inst, VIDC_SESSION_TYPE_DEC, HFI_VIDEO_CODEC_H264));
    struct hfi_framesize in = { HFI_BUFFER_INPUT, 96, 96 };
    set(HFI_PROPERTY_PARAM_FRAME_SIZE, &in);
    /* vdec_start_output: decide_core, vdec_set_properties (fw 5.4.32 is below 5.4.51: no sufficient-event property), output conf */
    struct hfi_videocores_usage_type cu = { VIDC_CORE_ID_1 };
    set(HFI_PROPERTY_CONFIG_VIDEOCORES_USAGE, &cu);
    u32 conceal = 0x08020010;
    set(HFI_PROPERTY_PARAM_VDEC_CONCEAL_COLOR, &conceal);
    output_conf(1920, 1080, 3133440, 3248128);
    /* vdec_verify_conf: the requirements of the outputs, then of the inputs */
    bufreq();
    bufreq();
    num_bufs(4);
    for (int i = 0; i < 5; i++) set_internal(internal[i]);
    run(core.ops->session_load_res(&inst));
    run(core.ops->session_start(&inst));
    etb(0, 0, 40000, 0);
    /* the event came: vdec_start_capture */
    output_conf(1920, 1080, 3133440, 3248128);
    num_bufs(4);
    release_internal(HFI_BUFFER_INTERNAL_SCRATCH(HFI_VERSION_4XX), 0x300000);
    release_internal(HFI_BUFFER_INTERNAL_SCRATCH_1(HFI_VERSION_4XX), 0x80000);
    for (int i = 0; i < 3; i++) set_internal(internal[i]);
    /* venus_helper_alloc_dpb_bufs: the reference type's count_min */
    bufreq();
    run(core.ops->session_continue(&inst));
    for (u32 i = 0; i < 6; i++) ftb(HFI_BUFFER_OUTPUT, 32 + i, 3248128);
    for (u32 i = 0; i < 2; i++) ftb(HFI_BUFFER_OUTPUT2, i, 3133440);
    etb(1, 0, 9000, 40000);
    /* a seek: vdec_stop_output, then vdec_start_output's SEEK branch with the capture side streaming */
    run(core.ops->session_flush(&inst, HFI_FLUSH_ALL));
    for (u32 i = 0; i < 6; i++) ftb(HFI_BUFFER_OUTPUT, 32 + i, 3248128);
    for (u32 i = 0; i < 2; i++) ftb(HFI_BUFFER_OUTPUT2, i, 3133440);
    etb(2, 0, 5000, 80000);
    /* the second sequence-changed event (1280 x 720) came with that ETB: vdec_event_change's flush (not waited),
     * vdec_stop_capture's in DRC (waited), then vdec_start_capture from CAPTURE_SETUP */
    run(core.ops->session_flush(&inst, HFI_FLUSH_OUTPUT));
    run(core.ops->session_flush(&inst, HFI_FLUSH_OUTPUT));
    output_conf(1280, 720, 1413120, 1507328);
    num_bufs(4);
    release_internal(HFI_BUFFER_INTERNAL_SCRATCH(HFI_VERSION_4XX), 0x300000);
    release_internal(HFI_BUFFER_INTERNAL_SCRATCH_1(HFI_VERSION_4XX), 0x80000);
    for (int i = 0; i < 3; i++) set_internal(internal[i]);
    bufreq();
    run(core.ops->session_continue(&inst));
    for (u32 i = 0; i < 6; i++) ftb(HFI_BUFFER_OUTPUT, 32 + i, 1507328);
    etb(0, HFI_BUFFERFLAG_EOS, 0, 0);
    run(core.ops->session_flush(&inst, HFI_FLUSH_ALL));
    run(core.ops->session_stop(&inst));
    run(core.ops->session_release_res(&inst));
    /* intbufs_free walks inst->internalbufs: intbufs_realloc took the scratch buffers out and put them back at its end. */
    release_internal(HFI_BUFFER_INTERNAL_PERSIST, 0x100000);
    release_internal(HFI_BUFFER_INTERNAL_PERSIST_1, 0x40000);
    release_internal(HFI_BUFFER_INTERNAL_SCRATCH(HFI_VERSION_4XX), 0x300000);
    release_internal(HFI_BUFFER_INTERNAL_SCRATCH_1(HFI_VERSION_4XX), 0x80000);
    run(core.ops->session_end(&inst));
}

int main(void) {
    static struct device dev;
    core.dev = &dev;
    core.res = &sc7180_res;
    core.base = regs;
    /* venus_assign_register_offsets, not V6. */
    core.vbif_base = regs + VBIF_BASE;
    core.cpu_base = regs + CPU_BASE;
    core.cpu_cs_base = regs + CPU_CS_BASE;
    core.cpu_ic_base = regs + CPU_IC_BASE;
    core.wrapper_base = regs + WRAPPER_BASE;
    INIT_LIST_HEAD(&core.instances);
    init_completion(&core.done);

    /* hfi_create: the packetizer for this HFI version, then the device. */
    pkt_set_version(sc7180_res.hfi_version);
    if (venus_hfi_create(&core)) return 1;
    u32 *t = kva(table_da);
    printf("table");
    for (int i = 0; i < 6 + 14 * 3; i++) { printf(" "); print_value(t[i]); }
    printf("\n");
    /* core.c: hfi_core_resume(core, true) -> venus_resume -> venus_power_on, then hfi_core_init -> venus_core_init. */
    if (core.ops->resume(&core)) return 2;
    if (core.ops->core_init(&core)) return 3;
    interrupt(&core);
    printf("core error %u version %u.%u-%u\n", core.error, core.venus_ver.major, core.venus_ver.minor, core.venus_ver.rev);
    session();
    return 0;
}
