/* Kernel and V4L2 types that core.h embeds by value: shapes only, the HFI files never look inside them. Register and DMA access go to
 * main.c, which records every register write, answers reads as the hardware would, and hands out memory at known device addresses. */
#ifndef ORACLE_FAKE_TYPES_H
#define ORACLE_FAKE_TYPES_H
typedef struct { int counter; } atomic_t;
#define atomic_set(a, v) ((a)->counter = (v))
#define atomic_read(a) ((a)->counter)
#define atomic_inc(a) ((a)->counter++)
#define atomic_dec(a) ((a)->counter--)
typedef int wait_queue_head_t;
#define init_waitqueue_head(w) (void)(w)
#define wake_up(w) (void)(w)
#define wake_up_all(w) (void)(w)
struct work_struct { int x; };
struct delayed_work { struct work_struct work; };
#define INIT_WORK(w, f) (void)(w)
#define INIT_DELAYED_WORK(w, f) (void)(w)
#define schedule_delayed_work(w, d) 0
#define cancel_delayed_work_sync(w) 0
struct v4l2_device { int x; };
struct v4l2_ctrl_handler { int x; };
struct v4l2_fh { int x; };
struct v4l2_rect { s32 left, top; u32 width, height; };
struct v4l2_fract { u32 numerator, denominator; };
struct v4l2_timecode { u32 type, flags; u8 frames, seconds, minutes, hours, userbits[4]; };
struct v4l2_ctrl_hdr10_cll_info { u16 max_content_light_level, max_pic_average_light_level; };
struct v4l2_ctrl_hdr10_mastering_display { u16 display_primaries_x[3], display_primaries_y[3], white_point_x, white_point_y; u32 max_display_mastering_luminance, min_display_mastering_luminance; };
struct vb2_buffer { u32 index; u32 type; };
struct vb2_v4l2_buffer { struct vb2_buffer vb2_buf; u32 flags; u32 field; u32 sequence; };
struct ida { int x; };
struct file;
struct v4l2_fh *file_to_v4l2_fh(struct file *f);
#define VIDEO_MAX_FRAME 64
/* Register access: main.c records writes and answers reads. */
u32 oracle_readl(const volatile void *addr);
void oracle_writel(u32 value, volatile void *addr);
#define readl(a) oracle_readl(a)
#define readl_relaxed(a) oracle_readl(a)
#define writel(v, a) oracle_writel((v), (a))
#define writel_relaxed(v, a) oracle_writel((v), (a))
/* The polls of iopoll.h: read until the condition holds, at most a few thousand times, as the hardware model answers at once. */
#define readx_poll_timeout(op, addr, val, cond, sleep_us, timeout_us) \
    ({ int __n = 0; for (;;) { (val) = op(addr); if (cond) break; if (++__n > 4096) break; } (cond) ? 0 : -ETIMEDOUT; })
#define readl_poll_timeout(addr, val, cond, sleep_us, timeout_us) readx_poll_timeout(oracle_readl, addr, val, cond, sleep_us, timeout_us)
#define readl_poll_timeout_atomic readl_poll_timeout
#define DMA_ATTR_WRITE_COMBINE 1
void *oracle_dma_alloc(size_t size, dma_addr_t *da);
#define dma_alloc_attrs(dev, size, da, gfp, attrs) oracle_dma_alloc((size), (da))
#define dma_free_attrs(dev, size, kva, da, attrs) do { } while (0)
#define disable_irq(i) do { } while (0)
#define enable_irq(i) do { } while (0)
#define disable_irq_nosync(i) do { } while (0)
#define QCOM_SMEM_HOST_ANY -1
void *qcom_smem_get(unsigned host, unsigned item, size_t *size);
#define V4L2_BUF_FLAG_KEYFRAME 0x00000008
#define V4L2_BUF_FLAG_PFRAME 0x00000010
#define V4L2_BUF_FLAG_BFRAME 0x00000020
#define V4L2_BUF_FLAG_LAST 0x00100000
#define v4l2_fourcc(a, b, c, d) ((u32)(a) | ((u32)(b) << 8) | ((u32)(c) << 16) | ((u32)(d) << 24))
#define V4L2_PIX_FMT_H264 v4l2_fourcc('H', '2', '6', '4')
#define V4L2_PIX_FMT_HEVC v4l2_fourcc('H', 'E', 'V', 'C')
#define V4L2_PIX_FMT_VP8 v4l2_fourcc('V', 'P', '8', '0')
#define V4L2_PIX_FMT_VP9 v4l2_fourcc('V', 'P', '9', '0')
#define V4L2_PIX_FMT_MPEG2 v4l2_fourcc('M', 'P', 'G', '2')
#define for_each_set_bit(bit, addr, size) for ((bit) = 0; (bit) < (size); (bit)++) if (!(*(addr) & (1UL << (bit)))) {} else
#endif
