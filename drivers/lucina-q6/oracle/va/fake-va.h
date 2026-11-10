/* What Linux's VA macro and LPI pin functions need of the kernel, for the oracle. The regmap part is regmap.c's and regcache.c's code
 * for an MMIO map with a flat cache (`_regmap_write`, `_regmap_read`, `_regmap_update_bits`, `regcache_read`, `regcache_write`,
 * `regcache_default_sync`, `regcache_reg_needs_sync`, `regcache_sync`, `regcache_sync_region`, `regcache_mark_dirty`,
 * `regcache_cache_only`, `regcache_flat_read`, `regcache_flat_write`, `regmap_readable`, `regmap_writeable`, `regmap_volatile`), cut to
 * the paths an MMIO map with no patch, no ranges and no bus update_bits takes. Every access that reaches the hardware is printed. */
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stddef.h>
#include <errno.h>

typedef uint8_t u8;
typedef uint16_t u16;
typedef uint32_t u32;
typedef int32_t s32;
typedef uint64_t u64;
#define __iomem
#define BIT(n) (1UL << (n))
#define GENMASK(h, l) (((~0UL) << (l)) & (~0UL >> (sizeof(long) * 8 - 1 - (h))))
#define ARRAY_SIZE(a) (sizeof(a) / sizeof((a)[0]))
#define container_of(ptr, type, member) ((type *)((char *)(ptr) - offsetof(type, member)))
#define WARN_ON(x) (x)
#define dev_err(dev, ...) fprintf(stderr, __VA_ARGS__)
#define dev_dbg(dev, ...) do { } while (0)
#define __maybe_unused
#define for_each_set_bit(bit, addr, size) for ((bit) = 0; (bit) < (size); (bit)++) if (*(addr) & (1UL << (bit)))
#define set_bit(nr, addr) (*(addr) |= 1UL << (nr))
#define test_and_set_bit(nr, addr) ({ int _o = !!(*(addr) & (1UL << (nr))); *(addr) |= 1UL << (nr); _o; })
#define DECLARE_BITMAP(name, bits) unsigned long name[((bits) + 63) / 64]
#define __bf_shf(x) (__builtin_ffsll(x) - 1)
static inline void u32p_replace_bits(u32 *p, u32 val, u32 field) { *p = (*p & ~field) | ((val << __bf_shf(field)) & field); }

struct device { void *drvdata; };
static inline void *dev_get_drvdata(struct device *dev) { return dev->drvdata; }
struct clk { const char *name; };
struct clk_hw { int unused; };
struct mutex { int unused; };
#define mutex_lock(m) do { } while (0)
#define mutex_unlock(m) do { } while (0)

static inline int clk_prepare_enable(struct clk *clk) { if (clk) printf("clk %s on\n", clk->name); return 0; }
static inline void clk_disable_unprepare(struct clk *clk) { if (clk) printf("clk %s off\n", clk->name); }
static inline void usleep_range(unsigned long min, unsigned long max) { printf("delay %lu\n", min); }

/* regmap */
struct reg_default { unsigned int reg; unsigned int def; };
#define REG_MAX 0x7A8
struct regmap {
    struct device *dev;
    const char *name;
    unsigned int max_register;
    bool (*writeable_reg)(struct device *dev, unsigned int reg);
    bool (*readable_reg)(struct device *dev, unsigned int reg);
    bool (*volatile_reg)(struct device *dev, unsigned int reg);
    const struct reg_default *reg_defaults;
    unsigned int num_reg_defaults;
    bool cache_only, cache_bypass, cache_dirty, no_sync_defaults;
    unsigned int cache[REG_MAX / 4 + 1];
    unsigned int hw[REG_MAX / 4 + 1];
};

static bool regmap_writeable(struct regmap *map, unsigned int reg)
{
    if (reg > map->max_register) return false;
    return map->writeable_reg(map->dev, reg);
}
static bool regmap_readable(struct regmap *map, unsigned int reg)
{
    if (reg > map->max_register) return false;
    return map->readable_reg(map->dev, reg);
}
static bool regmap_volatile(struct regmap *map, unsigned int reg)
{
    if (!regmap_readable(map, reg)) return false;
    return map->volatile_reg(map->dev, reg);
}
static int regcache_read(struct regmap *map, unsigned int reg, unsigned int *value)
{
    if (!regmap_volatile(map, reg)) { *value = map->cache[reg / 4]; return 0; }
    return -EINVAL;
}
static int regcache_write(struct regmap *map, unsigned int reg, unsigned int value)
{
    if (!regmap_volatile(map, reg)) map->cache[reg / 4] = value;
    return 0;
}
static int hw_write(struct regmap *map, unsigned int reg, unsigned int val)
{
    printf("%s w %03x %08x\n", map->name, reg, val);
    map->hw[reg / 4] = val;
    return 0;
}
static int hw_read(struct regmap *map, unsigned int reg, unsigned int *val)
{
    *val = map->hw[reg / 4];
    printf("%s r %03x %08x\n", map->name, reg, *val);
    return 0;
}
static int _regmap_write(struct regmap *map, unsigned int reg, unsigned int val)
{
    if (!regmap_writeable(map, reg)) return -EIO;
    if (!map->cache_bypass) {
        int ret = regcache_write(map, reg, val);
        if (ret != 0) return ret;
        if (map->cache_only) { map->cache_dirty = true; return 0; }
    }
    return hw_write(map, reg, val);
}
static int _regmap_read(struct regmap *map, unsigned int reg, unsigned int *val)
{
    int ret;
    if (!map->cache_bypass) {
        ret = regcache_read(map, reg, val);
        if (ret == 0) return 0;
    }
    if (map->cache_only) return -EBUSY;
    if (!regmap_readable(map, reg)) return -EIO;
    ret = hw_read(map, reg, val);
    if (ret == 0 && !map->cache_bypass) regcache_write(map, reg, *val);
    return ret;
}
static int regmap_update_bits(struct regmap *map, unsigned int reg, unsigned int mask, unsigned int val)
{
    unsigned int tmp, orig;
    int ret = _regmap_read(map, reg, &orig);
    if (ret != 0) return ret;
    tmp = orig & ~mask;
    tmp |= val & mask;
    if (tmp != orig) ret = _regmap_write(map, reg, tmp);
    return ret;
}
static int regmap_write(struct regmap *map, unsigned int reg, unsigned int val) { return _regmap_write(map, reg, val); }
static int regmap_read(struct regmap *map, unsigned int reg, unsigned int *val) { return _regmap_read(map, reg, val); }
static int regcache_lookup_reg(struct regmap *map, unsigned int reg)
{
    for (unsigned int i = 0; i < map->num_reg_defaults; i++)
        if (map->reg_defaults[i].reg == reg) return i;
    return -ENOENT;
}
static bool regcache_reg_needs_sync(struct regmap *map, unsigned int reg, unsigned int val)
{
    int ret;
    if (!regmap_writeable(map, reg)) return false;
    if (!map->no_sync_defaults) return true;
    ret = regcache_lookup_reg(map, reg);
    if (ret >= 0 && val == map->reg_defaults[ret].def) return false;
    return true;
}
static int regcache_default_sync(struct regmap *map, unsigned int min, unsigned int max)
{
    for (unsigned int reg = min; reg <= max; reg += 4) {
        unsigned int val;
        int ret;
        if (regmap_volatile(map, reg) || !regmap_writeable(map, reg)) continue;
        ret = regcache_read(map, reg, &val);
        if (ret) return ret;
        if (!regcache_reg_needs_sync(map, reg, val)) continue;
        map->cache_bypass = true;
        ret = _regmap_write(map, reg, val);
        map->cache_bypass = false;
        if (ret) return ret;
    }
    return 0;
}
static int regcache_sync(struct regmap *map)
{
    int ret = 0;
    bool bypass = map->cache_bypass;
    if (!map->cache_dirty) goto out;
    ret = regcache_default_sync(map, 0, map->max_register);
    if (ret == 0) map->cache_dirty = false;
out:
    map->cache_bypass = bypass;
    map->no_sync_defaults = false;
    return ret;
}
static int regcache_sync_region(struct regmap *map, unsigned int min, unsigned int max)
{
    int ret = 0;
    bool bypass = map->cache_bypass;
    if (!map->cache_dirty) goto out;
    ret = regcache_default_sync(map, min, max);
out:
    map->cache_bypass = bypass;
    map->no_sync_defaults = false;
    return ret;
}
static void regcache_mark_dirty(struct regmap *map) { map->cache_dirty = true; map->no_sync_defaults = true; }
static void regcache_cache_only(struct regmap *map, bool enable) { map->cache_only = enable; }

/* ASoC */
struct snd_soc_component { struct device *dev; struct regmap *regmap; };
struct snd_soc_dapm_context { struct snd_soc_component *component; };
struct snd_soc_dapm_widget { struct snd_soc_dapm_context *dapm; int shift; };
struct snd_kcontrol { unsigned long private_value; struct snd_soc_dapm_widget *widget; };
struct snd_ctl_elem_value { union { struct { unsigned int item[2]; } enumerated; struct { long value[2]; } integer; } value; };
struct soc_enum { int reg; unsigned char shift_l; unsigned int mask; };
struct snd_soc_dai { struct snd_soc_component *component; int id; };
struct snd_pcm_substream { int unused; };
struct snd_pcm_hw_params { unsigned int rate; };
#define SND_SOC_DAPM_PRE_PMU 0x1
#define SND_SOC_DAPM_POST_PMU 0x2
#define SND_SOC_DAPM_PRE_PMD 0x4
#define SND_SOC_DAPM_POST_PMD 0x8
static inline unsigned int params_rate(const struct snd_pcm_hw_params *p) { return p->rate; }
static inline struct snd_soc_component *snd_soc_dapm_to_component(struct snd_soc_dapm_context *dapm) { return dapm->component; }
static inline void *snd_soc_component_get_drvdata(struct snd_soc_component *c) { return dev_get_drvdata(c->dev); }
static inline struct snd_soc_dapm_widget *snd_soc_dapm_kcontrol_to_widget(struct snd_kcontrol *k) { return k->widget; }
static inline int snd_soc_component_update_bits(struct snd_soc_component *c, unsigned int reg, unsigned int mask, unsigned int val)
{
    return regmap_update_bits(c->regmap, reg, mask, val);
}
static inline unsigned int snd_soc_component_read(struct snd_soc_component *c, unsigned int reg)
{
    unsigned int val = 0;
    regmap_read(c->regmap, reg, &val);
    return val;
}
static inline int snd_soc_component_write(struct snd_soc_component *c, unsigned int reg, unsigned int val)
{
    return regmap_write(c->regmap, reg, val);
}
/* `snd_soc_dapm_put_enum_double` -> `dapm_widget_update` -> `soc_dapm_update_bits`: the field, through the component's regmap. */
static inline int snd_soc_dapm_put_enum_double(struct snd_kcontrol *k, struct snd_ctl_elem_value *u)
{
    struct soc_enum *e = (struct soc_enum *)k->private_value;
    struct snd_soc_component *c = snd_soc_dapm_to_component(k->widget->dapm);
    return regmap_update_bits(c->regmap, e->reg, e->mask << e->shift_l, u->value.enumerated.item[0] << e->shift_l);
}

/* LPI pins: the TLMM region, printed by offset from its base. */
static u32 lpi_hw[0x20000 / 4];
static inline u32 ioread32(const void __iomem *addr)
{
    size_t off = (size_t)addr;
    printf("lpi r %05zx %08x\n", off, lpi_hw[off / 4]);
    return lpi_hw[off / 4];
}
static inline void iowrite32(u32 val, void __iomem *addr)
{
    size_t off = (size_t)addr;
    printf("lpi w %05zx %08x\n", off, val);
    lpi_hw[off / 4] = val;
}
struct pinctrl_dev { struct device *dev; void *drvdata; };
static inline void *pinctrl_dev_get_drvdata(struct pinctrl_dev *p) { return p->drvdata; }
struct pinctrl_pin_desc;
#define MAX_NR_GPIO 32
/* generic pinconf: a parameter in the low byte, its argument above */
enum pin_config_param {
    PIN_CONFIG_BIAS_DISABLE = 1, PIN_CONFIG_BIAS_PULL_DOWN, PIN_CONFIG_BIAS_BUS_HOLD, PIN_CONFIG_BIAS_PULL_UP,
    PIN_CONFIG_INPUT_ENABLE, PIN_CONFIG_LEVEL, PIN_CONFIG_DRIVE_STRENGTH, PIN_CONFIG_SLEW_RATE,
};
#define pinconf_to_config_param(c) ((enum pin_config_param)((c) & 0xff))
#define pinconf_to_config_argument(c) ((u32)((c) >> 8))
#define pinconf_to_config_packed(p, a) ((unsigned long)(((a) << 8) | (unsigned long)(p)))
static inline u32 u32_encode_bits(u32 v, u32 field) { return (v << __bf_shf(field)) & field; }
struct lpi_pingroup;
struct lpi_pinctrl;
/* Only SoundWire pins have a slew field; the DMIC states set none. */
static int lpi_config_set_slew_rate(struct lpi_pinctrl *pctrl, const struct lpi_pingroup *g, unsigned int group, unsigned int slew) { return -EINVAL; }
