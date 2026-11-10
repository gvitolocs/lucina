/* Two captures from DMIC0 into decimator 0 at 48 kHz, as ALSA and DAPM call Linux's VA macro and LPI pin driver for one, printed as
 * the register accesses that reach the hardware. va-src.c is Linux's code, cut out of lpass-va-macro.c, pinctrl-lpass-lpi.c and
 * pinctrl-sm8250-lpass-lpi.c by build.sh, unchanged. Lines that start with `--` mark what the DSP is sent meanwhile. */
#include "va-src.c"

static struct device va_dev;
static struct va_macro va;
static struct regmap map;
static struct snd_soc_component comp = { .dev = &va_dev, .regmap = &map };
static struct snd_soc_dapm_context dapm = { .component = &comp };
static struct clk mclk = { "mclk" }, macro_clk = { "macro" }, dcodec_clk = { "dcodec" };
static struct lpi_pinctrl lpi;
static struct device lpi_device = { .drvdata = &lpi };
static struct pinctrl_dev lpi_dev = { .dev = &lpi_device, .drvdata = &lpi };
/* The other way the boards are set up: `main-va linux-boards`. */
static bool linux_boards;
/* `main-va laptop`: this laptop's internal mic as Windows sets it up (codec record "LPI_DMIC_6"): DMIC5 into VA decimator 2, on LPI
 * gpio12 and 13, with the active pin states of the DSDT's PEP component 10. */
static bool laptop;
static int dmic = 0, dec = VA_MACRO_DEC0, clk_pin = 6, data_pin = 7, clk_func = LPI_MUX_dmic1_clk, data_func = LPI_MUX_dmic1_data;
static const struct lpi_pinctrl_variant_data lpi_data = { .groups = sm8250_groups, .ngroups = ARRAY_SIZE(sm8250_groups) };

static void widget_event(int (*event)(struct snd_soc_dapm_widget *, struct snd_kcontrol *, int), int shift, int what)
{
    struct snd_soc_dapm_widget w = { .dapm = &dapm, .shift = shift };
    event(&w, NULL, what);
}

/* `va_macro_probe` for the sm8250 data, with no `qcom,dmic-sample-rate` in the device tree: the cache from the defaults, no hardware
 * access, the divider Linux falls back to. Then runtime PM's autosuspend, three seconds later. */
static void probe(void)
{
    va_dev.drvdata = &va;
    va.dev = &va_dev;
    va.regmap = &map;
    va.mclk = &mclk;
    va.macro = &macro_clk;
    va.dcodec = &dcodec_clk;
    va.dmic_clk_div = VA_MACRO_CLK_DIV_2;
    /* va_macro_probe: the rate the device tree gives, else the fallback above with an error. */
    if (linux_boards && !va_macro_validate_dmic_sample_rate(600000, &va))
        va.dmic_clk_div = VA_MACRO_CLK_DIV_2;
    map.dev = &va_dev;
    map.name = "va";
    map.max_register = VA_MAX_OFFSET;
    map.writeable_reg = va_is_rw_register;
    map.readable_reg = va_is_readable_register;
    map.volatile_reg = va_is_volatile_register;
    map.reg_defaults = va_defaults;
    map.num_reg_defaults = ARRAY_SIZE(va_defaults);
    for (unsigned int i = 0; i < ARRAY_SIZE(va_defaults); i++) {
        map.cache[va_defaults[i].reg / 4] = va_defaults[i].def;
        map.hw[va_defaults[i].reg / 4] = va_defaults[i].def;
    }
    lpi.data = &lpi_data;
    lpi.tlmm_base = NULL;
    clk_prepare_enable(va.macro);
    clk_prepare_enable(va.dcodec);
    clk_prepare_enable(va.mclk);
    va_macro_runtime_suspend(&va_dev);
}

/* What userspace sets before it opens the stream: "VA DMIC MUX0" = DMIC0, "VA DEC0 MUX" = VA_DMIC, "VA_AIF3_CAP Mixer DEC0" = 1. */
static void controls(void)
{
    struct snd_soc_dapm_widget w = { .dapm = &dapm };
    struct soc_enum dmic0 = { CDC_VA_INP_MUX_ADC_MUX0_CFG0 + VA_MACRO_ADC_MUX_CFG_OFFSET * dec, 4, 0xF };
    struct soc_enum dec0 = { CDC_VA_INP_MUX_ADC_MUX0_CFG1 + VA_MACRO_ADC_MUX_CFG_OFFSET * dec, 0, 0x1 };
    struct snd_kcontrol k = { .widget = &w };
    struct snd_ctl_elem_value v = { 0 };
    k.private_value = (unsigned long)&dmic0;
    v.value.enumerated.item[0] = dmic + 1;
    va_macro_put_dec_enum(&k, &v);
    k.private_value = (unsigned long)&dec0;
    v.value.enumerated.item[0] = 0;
    snd_soc_dapm_put_enum_double(&k, &v);
    /* `va_macro_tx_mixer_put`: no register. */
    set_bit(dec, &va.active_ch_mask[VA_MACRO_AIF3_CAP]);
    va.active_ch_cnt[VA_MACRO_AIF3_CAP]++;
}

static void capture(void)
{
    struct snd_soc_dai dai = { .component = &comp, .id = VA_MACRO_AIF3_CAP };
    struct snd_pcm_hw_params params = { .rate = 48000 };
    printf("-- open\n");
    /* `__soc_pcm_open`: the pins' default state, then runtime PM. The sm8250 boards' VA macro state: dmic01 clock and data. */
    lpi_gpio_set_mux(&lpi_dev, clk_func, clk_pin);
    if (linux_boards) {
        /* kodiak.dtsi lpass_dmic01_clk: drive-strength 8, bias-disable; the map order is each node's mux, then its config. */
        unsigned long clk[] = { pinconf_to_config_packed(PIN_CONFIG_DRIVE_STRENGTH, 8), pinconf_to_config_packed(PIN_CONFIG_BIAS_DISABLE, 0) };
        lpi_config_set(&lpi_dev, 6, clk, 2);
    }
    if (laptop) {
        /* PEP {0x0C,1,1,1,1,3,0,1}: value 1, function 1, output, pull-down, 8 mA; as a pin state: output-high. */
        unsigned long clk[] = { pinconf_to_config_packed(PIN_CONFIG_DRIVE_STRENGTH, 8), pinconf_to_config_packed(PIN_CONFIG_BIAS_PULL_DOWN, 0), pinconf_to_config_packed(PIN_CONFIG_LEVEL, 1) };
        lpi_config_set(&lpi_dev, clk_pin, clk, 3);
    }
    lpi_gpio_set_mux(&lpi_dev, data_func, data_pin);
    if (linux_boards) {
        /* lpass_dmic01_data: drive-strength 8, bias-pull-down. */
        unsigned long data[] = { pinconf_to_config_packed(PIN_CONFIG_DRIVE_STRENGTH, 8), pinconf_to_config_packed(PIN_CONFIG_BIAS_PULL_DOWN, 0) };
        lpi_config_set(&lpi_dev, 7, data, 2);
    }
    if (laptop) {
        /* PEP {0x0D,1,1,0,1,3,0,1}: function 1, input, pull-down, 8 mA. */
        unsigned long data[] = { pinconf_to_config_packed(PIN_CONFIG_DRIVE_STRENGTH, 8), pinconf_to_config_packed(PIN_CONFIG_BIAS_PULL_DOWN, 0), pinconf_to_config_packed(PIN_CONFIG_INPUT_ENABLE, 1) };
        lpi_config_set(&lpi_dev, data_pin, data, 3);
    }
    va_macro_runtime_resume(&va_dev);
    va_macro_hw_params(NULL, &params, &dai);
    printf("-- afe port config, port start\n");
    /* BE prepare done: DAPM stream start. supply, aif_out, mux (PRE then POST), mixer, adc; then the unmute. */
    fsgen_gate_enable(&va.hw);
    widget_event(va_macro_enable_dec, dec, SND_SOC_DAPM_PRE_PMU);
    widget_event(va_macro_enable_dec, dec, SND_SOC_DAPM_POST_PMU);
    widget_event(va_macro_enable_dmic, dmic, SND_SOC_DAPM_PRE_PMU);
    va_macro_digital_mute(&dai, 0, 1);
    printf("-- asm map, open read, adm open, matrix, encoder config, reads, run\n");
    printf("-- eos\n");
    /* hw_free: the BE's mute, then DAPM stream stop: adc, mux, supply. */
    va_macro_digital_mute(&dai, 1, 1);
    widget_event(va_macro_enable_dmic, dmic, SND_SOC_DAPM_POST_PMD);
    widget_event(va_macro_enable_dec, dec, SND_SOC_DAPM_POST_PMD);
    fsgen_gate_disable(&va.hw);
    printf("-- afe port stop, asm close, unmap, adm close\n");
    /* No sleep state for the pins. Runtime PM suspends the macro three seconds after the close. */
    va_macro_runtime_suspend(&va_dev);
}

int main(int argc, char **argv)
{
    linux_boards = argc > 1 && !strcmp(argv[1], "linux-boards");
    laptop = argc > 1 && !strcmp(argv[1], "laptop");
    if (laptop) {
        dmic = 5;
        dec = VA_MACRO_DEC2;
        clk_pin = 12;
        data_pin = 13;
        clk_func = LPI_MUX_dmic3_clk;
        data_func = LPI_MUX_dmic3_data;
    }
    probe();
    controls();
    capture();
    if (!linux_boards && !laptop)
        capture();
    return 0;
}
