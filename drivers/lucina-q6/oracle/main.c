/* Runs Linux's own q6afe.c, q6adm.c, q6asm.c and q6core.c through one playback on tertiary MI2S, called with what sc7180.c, q6afe-dai.c,
 * q6routing.c and q6asm-dai.c pass them for that stream, in the order ALSA calls those. Prints `<name> <hex>` for each packet Linux
 * sends, for tests/oracle.rs. The model DSP here only answers, the way the callbacks expect. */
#include <linux/soc/qcom/apr.h>
#include <dt-bindings/sound/qcom,q6afe.h>
#include "q6adm.h"
#include "q6afe.h"
#include "q6asm.h"
#include "q6core.h"

extern struct apr_driver *oracle_afe, *oracle_adm, *oracle_asm, *oracle_core;
#define NULL_COPP_TOPOLOGY 0x00010312
#define ASM_CMD_SHARED_MEM_MAP_REGIONS 0x00010D92
#define ASM_CMDRSP_SHARED_MEM_MAP_REGIONS 0x00010D93
#define ASM_DATA_EVENT_WRITE_DONE_V2 0x00010D99
#define ASM_DATA_EVENT_READ_DONE_V2 0x00010D9A
#define ASM_DATA_CMD_READ_V2 0x00010DAC
#define ADM_CMD_DEVICE_OPEN_V5 0x00010326
#define ADM_CMDRSP_DEVICE_OPEN_V5 0x00010329
#define AVCS_CMD_GET_FWK_VERSION 0x001292c
#define AVCS_CMD_ADSP_EVENT_GET_STATE 0x0001290C
#define AVCS_CMDRSP_ADSP_EVENT_GET_STATE 0x0001290D
#define AVCS_CMDRSP_GET_FWK_VERSION 0x001292d
#define ASM_DATA_CMD_WRITE_V2 0x00010DAB
#define ASM_SESSION_CMD_RUN_V2 0x00010DAA
#define ASM_DATA_CMD_EOS 0x00010BDB
#define HANDLE 0xabcd1234u
#define COPP 0x11
#define ADDR 0x23456000ull
#define IOMMU_STREAM 0x1001
#define SID_MASK_DEFAULT 0xF

static struct apr_device core, afe, adm, as;
static const char *label;
static const char **next_labels;
static const char *mic_start_labels[] = { "mic_port_config", "mic_port_start", NULL };
void oracle_set_cdc_dma_lanes(struct q6afe_port *port, u16 mask);
static const char *mic_lane2_labels[] = { "mic_port_config_lane2", "mic_port_start_lane2", NULL };
static struct apr_driver *driver_of(struct apr_device *a) { return a == &core ? oracle_core : a == &afe ? oracle_afe : a == &adm ? oracle_adm : oracle_asm; }

static void answer(struct apr_device *a, struct apr_hdr *cmd, u32 opcode, void *payload, int size) {
	struct apr_resp_pkt resp;
	memset(&resp, 0, sizeof resp);
	resp.hdr = *cmd;
	resp.hdr.src_port = cmd->dest_port;
	resp.hdr.dest_port = cmd->src_port;
	resp.hdr.opcode = opcode;
	resp.payload = payload;
	resp.payload_size = size;
	driver_of(a)->callback(a, &resp);
}

int apr_send_pkt(struct apr_device *adev, struct apr_pkt *pkt) {
	struct apr_hdr *hdr = &pkt->hdr;
	const u8 *b = (const u8 *)pkt;
	/* As apr.c fills them. */
	hdr->src_domain = APR_DOMAIN_APPS;
	hdr->src_svc = adev->svc.id;
	hdr->dest_domain = adev->domain_id;
	hdr->dest_svc = adev->svc.id;
	if (adev == &core) {
		/* q6core.c leaves these out of a packet on its stack: whatever was there goes to the DSP. */
		hdr->src_port = hdr->dest_port = 0;
		hdr->token = 0;
	}
	if (next_labels && *next_labels)
		label = *next_labels++;
	printf("%s ", label ? label : "UNNAMED");
	for (int i = 0; i < hdr->pkt_size; i++)
		printf("%02x", b[i]);
	printf("\n");
	label = NULL;
	u32 op = hdr->opcode;
	if (op == ASM_CMD_SHARED_MEM_MAP_REGIONS) {
		u32 word[2] = { HANDLE, 0 };
		answer(adev, hdr, ASM_CMDRSP_SHARED_MEM_MAP_REGIONS, word, sizeof word);
	} else if (op == ADM_CMD_DEVICE_OPEN_V5) {
		struct { u32 status; u16 copp; u16 reserved; } __packed rsp = { 0, COPP, 0 };
		answer(adev, hdr, ADM_CMDRSP_DEVICE_OPEN_V5, &rsp, sizeof rsp);
	} else if (op == 0x000100f4) {
		u32 rsp[2] = { 0, 0 };
		answer(adev, hdr, 0x000100f5, rsp, sizeof rsp);
	} else if (op == AVCS_CMD_GET_FWK_VERSION) {
		struct { u32 a; u32 b; u32 c; u32 d; u32 e; u32 services; } rsp = { 0 };
		answer(adev, hdr, AVCS_CMDRSP_GET_FWK_VERSION, &rsp, sizeof rsp);
	} else if (op == AVCS_CMD_ADSP_EVENT_GET_STATE) {
		u32 word[2] = { 1, 0 };
		answer(adev, hdr, AVCS_CMDRSP_ADSP_EVENT_GET_STATE, word, sizeof word);
	} else if (op != ASM_DATA_CMD_WRITE_V2 && op != ASM_DATA_CMD_READ_V2 && op != ASM_DATA_CMD_EOS) {
		struct aprv2_ibasic_rsp_result_t r = { op, 0 };
		answer(adev, hdr, APR_BASIC_RSP_RESULT, &r, sizeof r);
	}
	return hdr->pkt_size;
}

static void up(struct apr_device *a, int svc, const char *name) {
	a->svc_id = svc;
	a->svc.id = svc;
	a->domain_id = APR_DOMAIN_ADSP;
	label = name;
	if (driver_of(a)->probe(a))
		exit(1);
}

static int must(const char *what, long rc) {
	if (rc < 0) {
		fprintf(stderr, "oracle: %s failed: %ld\n", what, rc);
		exit(1);
	}
	return rc;
}

static void noop(uint32_t opcode, uint32_t token, void *payload, void *priv) { }

int main(void) {
	up(&core, APR_SVC_ADSP_CORE, NULL);
	up(&afe, APR_SVC_AFE, "avcs_fwk_version");
	up(&adm, APR_SVC_ADM, NULL);
	up(&as, APR_SVC_ASM, NULL);
	struct device afe_dais = { .parent = &afe.dev }, asm_dais = { .parent = &as.dev }, routing = { .parent = &adm.dev };

	label = "avcs_get_state";
	if (!q6core_is_adsp_ready())
		exit(1);

	/* sc7180_qdsp_snd_startup, through q6afe_mi2s_set_sysclk */
	struct q6afe_port *port = q6afe_port_get_from_id(&afe_dais, TERTIARY_MI2S_RX);
	label = "clock_on";
	must("clock", q6afe_port_set_sysclk(port, Q6AFE_LPASS_CLK_ID_TER_MI2S_IBIT, Q6AFE_LPASS_CLK_ATTRIBUTE_COUPLE_NO,
					    Q6AFE_LPASS_CLK_ROOT_DEFAULT, 1536000, SNDRV_PCM_STREAM_PLAYBACK));

	/* q6i2s_hw_params with sc7180_qdsp_be_hw_params_fixup, qcom,sd-lines = <1> of dai@20; q6i2s_set_fmt with SND_SOC_DAIFMT_BP_FP */
	struct q6afe_i2s_cfg i2s = { .sample_rate = 48000, .bit_width = 16, .num_channels = 2, .sd_line_mask = BIT(1), .fmt = SND_SOC_DAIFMT_BP_FP };
	must("i2s prepare", q6afe_i2s_port_prepare(port, &i2s));
	/* q6afe_port_start sends two packets. */
	static const char *start_labels[] = { "port_config", "port_start", NULL };
	next_labels = start_labels;
	must("port start", q6afe_port_start(port));
	next_labels = NULL;

	/* q6asm_dai_open: dma_buffer.addr | (sid << 32) */
	long long sid = IOMMU_STREAM & SID_MASK_DEFAULT;
	phys_addr_t phys = ADDR | ((u64)sid << 32);
	struct audio_client *ac = q6asm_audio_client_alloc(&asm_dais, noop, NULL, 0, LEGACY_PCM_MODE);
	must("client", IS_ERR(ac) ? PTR_ERR(ac) : 0);

	/* q6asm_dai_prepare and q6routing_stream_open */
	label = "map_memory";
	must("map", q6asm_map_memory_regions(SNDRV_PCM_STREAM_PLAYBACK, ac, phys, 3840, 8));
	label = "open_write";
	must("open", q6asm_open_write(ac, 1, FORMAT_LINEAR_PCM, 0, 16, false));
	label = "adm_open";
	struct q6copp *copp = q6adm_open(&routing, TERTIARY_MI2S_RX, ADM_PATH_PLAYBACK, 48000, 2, NULL_COPP_TOPOLOGY, LEGACY_PCM_MODE, 16, 0, 0);
	must("adm open", IS_ERR_OR_NULL(copp) ? -1 : 0);
	struct route_payload route = { .num_copps = 1, .session_id = 1 };
	route.port_id[0] = TERTIARY_MI2S_RX;
	route.copp_idx[0] = q6adm_get_copp_id(copp);
	label = "matrix";
	must("matrix", q6adm_matrix_map(&routing, ADM_PATH_PLAYBACK, route, LEGACY_PCM_MODE));
	label = "media_format";
	must("format", q6asm_media_format_block_multi_ch_pcm(ac, 1, 48000, 2, NULL, 16));

	/* q6asm_dai_ack, before the trigger */
	static const char *writes[] = { "write_slot0", "write_slot1", "write_slot2" };
	for (int i = 0; i < 3; i++) {
		label = writes[i];
		must("write", q6asm_write_async(ac, 1, 3840, 0, 0, 0));
	}
	/* q6asm_dai_trigger */
	label = "run";
	must("run", q6asm_run_nowait(ac, 1, 0, 0, 0));
	label = "write_slot3";
	must("write", q6asm_write_async(ac, 1, 3840, 0, 0, 0));

	/* What q6asm_stream_callback accepts for slot 2. If Linux refused it, hw_ptr would stay. */
	u32 done[4] = { lower_32_bits(phys + 2 * 3840), upper_32_bits(phys + 2 * 3840), HANDLE, 0 };
	struct apr_hdr h = { 0 };
	h.token = 2 | (3840 << 16);
	h.dest_port = (1 << 8) | 1;
	h.src_port = h.dest_port;
	answer(&as, &h, ASM_DATA_EVENT_WRITE_DONE_V2, done, sizeof done);
	if (q6asm_get_hw_pointer(ac, SNDRV_PCM_STREAM_PLAYBACK) != 3) {
		fprintf(stderr, "oracle: Linux refused the write-done\n");
		exit(1);
	}
	printf("write_done_slot2 %08x%08x\n", __builtin_bswap32(done[0]), __builtin_bswap32(done[1]));

	/* q6asm_dai_trigger on stop, then dpcm_fe_dai_shutdown: the back end first */
	label = "eos";
	must("eos", q6asm_cmd_nowait(ac, 1, CMD_EOS));
	label = "port_stop";
	must("stop", q6afe_port_stop(port));
	label = "clock_off";
	must("clock off", q6afe_port_set_sysclk(port, Q6AFE_LPASS_CLK_ID_TER_MI2S_IBIT, Q6AFE_LPASS_CLK_ATTRIBUTE_COUPLE_NO,
						Q6AFE_LPASS_CLK_ROOT_DEFAULT, 0, SNDRV_PCM_STREAM_PLAYBACK));
	label = "close";
	must("close", q6asm_cmd(ac, 1, CMD_CLOSE));
	label = "unmap";
	must("unmap", q6asm_unmap_memory_regions(SNDRV_PCM_STREAM_PLAYBACK, ac));
	label = "adm_close";
	must("adm close", q6adm_close(&routing, copp));

	/* The internal microphone: VA_CODEC_DMA_TX_2, mono, as q6afe-dai.c, q6asm-dai.c and q6routing.c call these for a capture. */
	/* va_macro_probe's macro and dcodec clocks, and the clock controller's bus clock: q6afe-clocks.c reaches these */
	u32 handle = 0;
	label = "mic_vote_macro";
	must("vote macro", q6afe_vote_lpass_core_hw(&afe_dais, Q6AFE_LPASS_CORE_HW_MACRO_BLOCK, "LPASS_HW_MACRO", &handle));
	label = "mic_vote_dcodec";
	must("vote dcodec", q6afe_vote_lpass_core_hw(&afe_dais, Q6AFE_LPASS_CORE_HW_DCODEC_BLOCK, "LPASS_HW_DCODEC", &handle));
	label = "mic_bus_clock";
	must("bus clock", q6afe_set_lpass_clock(&afe_dais, 0x30D, Q6AFE_LPASS_CLK_ATTRIBUTE_COUPLE_NO, Q6AFE_LPASS_CLK_ROOT_DEFAULT, 19200000));
	/* va_macro_probe: clk_set_rate(mclk, 19200000), clk_prepare_enable(mclk); sm8250.dtsi's mclk is LPASS_CLK_ID_TX_CORE_MCLK */
	label = "mic_mclk";
	must("mclk", q6afe_set_lpass_clock(&afe_dais, Q6AFE_LPASS_CLK_ID_TX_CORE_MCLK, Q6AFE_LPASS_CLK_ATTRIBUTE_COUPLE_NO, Q6AFE_LPASS_CLK_ROOT_DEFAULT, 19200000));
	struct q6afe_port *va = q6afe_port_get_from_id(&afe_dais, VA_CODEC_DMA_TX_2);
	struct q6afe_cdc_dma_cfg dma = { .sample_rate = 48000, .bit_width = 16, .data_format = 0, .num_channels = 1, .active_channels_mask = 0 };
	q6afe_cdc_dma_port_prepare(va, &dma);
	next_labels = mic_start_labels;
	must("va start", q6afe_port_start(va));
	next_labels = NULL;
	struct audio_client *rec = q6asm_audio_client_alloc(&asm_dais, noop, NULL, 1, LEGACY_PCM_MODE);
	must("rec client", IS_ERR(rec) ? PTR_ERR(rec) : 0);
	phys_addr_t rec_phys = (ADDR + 0x10000) | ((u64)sid << 32);
	label = "mic_map_memory";
	must("rec map", q6asm_map_memory_regions(SNDRV_PCM_STREAM_CAPTURE, rec, rec_phys, 1920, 8));
	label = "mic_open_read";
	must("open read", q6asm_open_read(rec, 1, FORMAT_LINEAR_PCM, 16));
	label = "mic_adm_open";
	struct q6copp *rcopp = q6adm_open(&routing, VA_CODEC_DMA_TX_2, ADM_PATH_LIVE_REC, 48000, 1, NULL_COPP_TOPOLOGY, LEGACY_PCM_MODE, 16, 0, 0);
	must("rec adm open", IS_ERR_OR_NULL(rcopp) ? -1 : 0);
	struct route_payload rroute = { .num_copps = 1, .session_id = 2 };
	rroute.port_id[0] = VA_CODEC_DMA_TX_2;
	rroute.copp_idx[0] = q6adm_get_copp_id(rcopp);
	label = "mic_matrix";
	must("rec matrix", q6adm_matrix_map(&routing, ADM_PATH_LIVE_REC, rroute, LEGACY_PCM_MODE));
	label = "mic_encoder_config";
	must("enc cfg", q6asm_enc_cfg_blk_pcm_format_support(rec, 1, 48000, 1, 16));
	static const char *reads[] = { "mic_read_slot0", "mic_read_slot1", "mic_read_slot2", "mic_read_slot3", "mic_read_slot4", "mic_read_slot5", "mic_read_slot6", "mic_read_slot7" };
	for (int i = 0; i < 8; i++) {
		label = reads[i];
		must("read", q6asm_read(rec, 1));
	}
	label = "mic_run";
	must("rec run", q6asm_run_nowait(rec, 1, 0, 0, 0));
	u32 rdone[3] = { 0, lower_32_bits(rec_phys + 3 * 1920), upper_32_bits(rec_phys + 3 * 1920) };
	struct apr_hdr rh = { 0 };
	rh.token = 3;
	rh.dest_port = (2 << 8) | 1;
	rh.src_port = rh.dest_port;
	answer(&as, &rh, ASM_DATA_EVENT_READ_DONE_V2, rdone, sizeof rdone);
	if (q6asm_get_hw_pointer(rec, SNDRV_PCM_STREAM_CAPTURE) != 4) {
		fprintf(stderr, "oracle: Linux refused the read-done\n");
		exit(1);
	}
	printf("mic_read_done_slot3 %08x%08x%08x\n", __builtin_bswap32(rdone[0]), __builtin_bswap32(rdone[1]), __builtin_bswap32(rdone[2]));
	label = "mic_eos";
	must("rec eos", q6asm_cmd_nowait(rec, 1, CMD_EOS));
	label = "mic_port_stop";
	must("va stop", q6afe_port_stop(va));
	label = "mic_close";
	must("rec close", q6asm_cmd(rec, 1, CMD_CLOSE));
	label = "mic_unmap";
	must("rec unmap", q6asm_unmap_memory_regions(SNDRV_PCM_STREAM_CAPTURE, rec));
	label = "mic_adm_close";
	must("rec adm close", q6adm_close(&routing, rcopp));

	/* This laptop's internal mic as Windows starts it: VA decimator 2, so codec DMA lane 2 (qcauddev7180.sys: 1 << the decimator's place in
	 * 0xB025's list of the four VA decimators), one channel. q6afe_cdc_dma_port_prepare keeps a mask it is given. */
	struct q6afe_cdc_dma_cfg dma2 = { .sample_rate = 48000, .bit_width = 16, .data_format = 0, .num_channels = 1, .active_channels_mask = 1 << 2 };
	q6afe_cdc_dma_port_prepare(va, &dma2);
	oracle_set_cdc_dma_lanes(va, 1 << 2);
	next_labels = mic_lane2_labels;
	must("va start lane 2", q6afe_port_start(va));
	next_labels = NULL;
	return 0;
}
