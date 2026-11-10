// SPDX-License-Identifier: GPL-2.0
/*
 * Golden-data generator for drivers/lucina-qrtr.
 *
 * Uses Linux's own struct definitions: struct qrtr_ctrl_pkt from include/uapi/linux/qrtr.h, and
 * struct qrtr_hdr_v1 and qrtr_hdr_v2 together with their constants, cut out of net/qrtr/af_qrtr.c and ns.c by
 * build.sh. It assembles packets the way qrtr_node_enqueue() does (push the header, size = payload length,
 * pad to a multiple of four) and prints them:
 *
 *   packets.txt    "<case> <hex>"
 *   constants.txt  "<NAME> <value>": numbers, sizes and field offsets of the structs
 *
 * af_qrtr.c itself cannot be linked without the networking stack, so the packet *building* below is typed in
 * from qrtr_node_enqueue() (about ten lines); the layouts and numbers are not.
 */
#include <uapi/linux/qrtr.h>
#include "extracted.h"

static FILE *pk;

static void hex(const u8 *p, size_t n)
{
	for (size_t i = 0; i < n; i++)
		fprintf(pk, "%02x", p[i]);
}

static size_t align4(size_t n)
{
	return (n + 3) & ~(size_t)3;
}

/* qrtr_node_enqueue() */
static void v1(const char *name, u32 type, u32 src_node, u32 src_port, u32 dst_node, u32 dst_port,
	       int confirm_rx, const void *payload, size_t len)
{
	size_t total = sizeof(struct qrtr_hdr_v1) + align4(len);
	u8 *buf = calloc(1, total);
	struct qrtr_hdr_v1 *hdr = (struct qrtr_hdr_v1 *)buf;

	hdr->version = cpu_to_le32(QRTR_PROTO_VER_1);
	hdr->type = cpu_to_le32(type);
	hdr->src_node_id = cpu_to_le32(src_node);
	hdr->src_port_id = cpu_to_le32(src_port);
	hdr->dst_node_id = cpu_to_le32(dst_node);
	hdr->dst_port_id = cpu_to_le32(dst_port);
	hdr->size = cpu_to_le32(len);
	hdr->confirm_rx = cpu_to_le32(!!confirm_rx);
	memcpy(buf + sizeof(*hdr), payload, len);

	fprintf(pk, "%s ", name);
	hex(buf, total);
	fputc('\n', pk);
	free(buf);
}

/* The v2 layout, which Linux receives but never builds: fill the struct the same way. */
static void v2(const char *name, u8 type, u8 flags, const void *opt, u8 optlen, u16 src_node, u16 src_port,
	       u16 dst_node, u16 dst_port, const void *payload, size_t len)
{
	size_t total = sizeof(struct qrtr_hdr_v2) + optlen + align4(len);
	u8 *buf = calloc(1, total);
	struct qrtr_hdr_v2 *hdr = (struct qrtr_hdr_v2 *)buf;

	hdr->version = QRTR_PROTO_VER_2;
	hdr->type = type;
	hdr->flags = flags;
	hdr->optlen = optlen;
	hdr->size = cpu_to_le32(len);
	hdr->src_node_id = cpu_to_le16(src_node);
	hdr->src_port_id = cpu_to_le16(src_port);
	hdr->dst_node_id = cpu_to_le16(dst_node);
	hdr->dst_port_id = cpu_to_le16(dst_port);
	memcpy(buf + sizeof(*hdr), opt, optlen);
	memcpy(buf + sizeof(*hdr) + optlen, payload, len);

	fprintf(pk, "%s ", name);
	hex(buf, total);
	fputc('\n', pk);
	free(buf);
}

static void ctrl_bytes(const char *name, const struct qrtr_ctrl_pkt *p)
{
	fprintf(pk, "%s ", name);
	hex((const u8 *)p, sizeof(*p));
	fputc('\n', pk);
}

static struct qrtr_ctrl_pkt server_pkt(u32 cmd, u32 service, u32 instance, u32 node, u32 port)
{
	struct qrtr_ctrl_pkt p;

	memset(&p, 0, sizeof(p));
	p.cmd = cpu_to_le32(cmd);
	p.server.service = cpu_to_le32(service);
	p.server.instance = cpu_to_le32(instance);
	p.server.node = cpu_to_le32(node);
	p.server.port = cpu_to_le32(port);
	return p;
}

static struct qrtr_ctrl_pkt client_pkt(u32 cmd, u32 node, u32 port)
{
	struct qrtr_ctrl_pkt p;

	memset(&p, 0, sizeof(p));
	p.cmd = cpu_to_le32(cmd);
	p.client.node = cpu_to_le32(node);
	p.client.port = cpu_to_le32(port);
	return p;
}

#define C(n) fprintf(cst, "%s %llu\n", #n, (unsigned long long)(n))

static void constants(FILE *cst)
{
	C(QRTR_PROTO_VER_1);
	C(QRTR_PROTO_VER_2);
	C(QRTR_NODE_BCAST);
	C(QRTR_PORT_CTRL);
	C(QRTR_PORT_CTRL_LEGACY);
	fprintf(cst, "QRTR_EP_NID_AUTO %llu\n", (unsigned long long)(unsigned int)QRTR_EP_NID_AUTO);
	C(QRTR_MIN_EPH_SOCKET);
	C(QRTR_MAX_EPH_SOCKET);
	C(QRTR_FLAGS_CONFIRM_RX);
	C(QRTR_TX_FLOW_HIGH);
	C(QRTR_TX_FLOW_LOW);
	C(QRTR_NS_MAX_NODES);
	C(QRTR_NS_MAX_SERVERS);
	C(QRTR_NS_MAX_LOOKUPS);
	C(QRTR_TYPE_DATA);
	C(QRTR_TYPE_HELLO);
	C(QRTR_TYPE_BYE);
	C(QRTR_TYPE_NEW_SERVER);
	C(QRTR_TYPE_DEL_SERVER);
	C(QRTR_TYPE_DEL_CLIENT);
	C(QRTR_TYPE_RESUME_TX);
	C(QRTR_TYPE_EXIT);
	C(QRTR_TYPE_PING);
	C(QRTR_TYPE_NEW_LOOKUP);
	C(QRTR_TYPE_DEL_LOOKUP);
	C(sizeof(struct qrtr_hdr_v1));
	C(sizeof(struct qrtr_hdr_v2));
	C(sizeof(struct qrtr_ctrl_pkt));
	C(offsetof(struct qrtr_hdr_v1, version));
	C(offsetof(struct qrtr_hdr_v1, type));
	C(offsetof(struct qrtr_hdr_v1, src_node_id));
	C(offsetof(struct qrtr_hdr_v1, src_port_id));
	C(offsetof(struct qrtr_hdr_v1, confirm_rx));
	C(offsetof(struct qrtr_hdr_v1, size));
	C(offsetof(struct qrtr_hdr_v1, dst_node_id));
	C(offsetof(struct qrtr_hdr_v1, dst_port_id));
	C(offsetof(struct qrtr_hdr_v2, version));
	C(offsetof(struct qrtr_hdr_v2, type));
	C(offsetof(struct qrtr_hdr_v2, flags));
	C(offsetof(struct qrtr_hdr_v2, optlen));
	C(offsetof(struct qrtr_hdr_v2, size));
	C(offsetof(struct qrtr_hdr_v2, src_node_id));
	C(offsetof(struct qrtr_hdr_v2, src_port_id));
	C(offsetof(struct qrtr_hdr_v2, dst_node_id));
	C(offsetof(struct qrtr_hdr_v2, dst_port_id));
	C(offsetof(struct qrtr_ctrl_pkt, cmd));
	C(offsetof(struct qrtr_ctrl_pkt, server.service));
	C(offsetof(struct qrtr_ctrl_pkt, server.instance));
	C(offsetof(struct qrtr_ctrl_pkt, server.node));
	C(offsetof(struct qrtr_ctrl_pkt, server.port));
	C(offsetof(struct qrtr_ctrl_pkt, client.node));
	C(offsetof(struct qrtr_ctrl_pkt, client.port));
}

int main(int argc, char **argv)
{
	const char *dir = argc > 1 ? argv[1] : ".";
	char path[512];
	FILE *cst;
	struct qrtr_ctrl_pkt p;

	snprintf(path, sizeof(path), "%s/packets.txt", dir);
	pk = fopen(path, "w");
	snprintf(path, sizeof(path), "%s/constants.txt", dir);
	cst = fopen(path, "w");
	if (!pk || !cst) {
		perror("open");
		return 1;
	}
	fprintf(pk, "# generated by drivers/lucina-qrtr/oracle (Linux qrtr structs); do not edit\n");
	fprintf(cst, "# generated by drivers/lucina-qrtr/oracle (Linux qrtr structs); do not edit\n");
	constants(cst);
	fclose(cst);

	/* the 20-byte control payloads */
	p = server_pkt(QRTR_TYPE_NEW_SERVER, 69, 1, 0, 0x2a);
	ctrl_bytes("ctrl_new_server", &p);
	p = client_pkt(QRTR_TYPE_RESUME_TX, 1, 0x4001);
	ctrl_bytes("ctrl_resume_tx", &p);
	p = client_pkt(QRTR_TYPE_BYE, 0, 0);
	ctrl_bytes("ctrl_bye", &p);
	p = server_pkt(QRTR_TYPE_NEW_SERVER, 0xdeadbeef, 0x01020304, 0x0a0b0c0d, 0xf0e0d0c0);
	ctrl_bytes("ctrl_server_all_bits", &p);
	memset(&p, 0, sizeof(p));
	p.cmd = cpu_to_le32(QRTR_TYPE_HELLO);
	ctrl_bytes("ctrl_hello", &p);

	/* name service traffic between this node (1) and the modem (0) */
	p = server_pkt(QRTR_TYPE_HELLO, 0, 0, 0, 0);
	v1("hello_to_unknown_node", QRTR_TYPE_HELLO, 1, QRTR_PORT_CTRL, (u32)QRTR_EP_NID_AUTO, QRTR_PORT_CTRL, 0, &p, sizeof(p));
	v1("hello_from_node0_to_unknown_node", QRTR_TYPE_HELLO, 0, QRTR_PORT_CTRL, (u32)QRTR_EP_NID_AUTO, QRTR_PORT_CTRL, 0, &p, sizeof(p));
	v1("hello_to_node0", QRTR_TYPE_HELLO, 1, QRTR_PORT_CTRL, 0, QRTR_PORT_CTRL, 0, &p, sizeof(p));
	v1("hello_from_node0", QRTR_TYPE_HELLO, 0, QRTR_PORT_CTRL, 1, QRTR_PORT_CTRL, 0, &p, sizeof(p));
	p = server_pkt(QRTR_TYPE_NEW_SERVER, 69, 1, 0, 0x2a);
	v1("new_server_from_modem", QRTR_TYPE_NEW_SERVER, 0, QRTR_PORT_CTRL, 1, QRTR_PORT_CTRL, 0, &p, sizeof(p));
	p = server_pkt(QRTR_TYPE_NEW_SERVER, 69, 1, 1, 0x4001);
	v1("new_server_announce_local", QRTR_TYPE_NEW_SERVER, 1, QRTR_PORT_CTRL, 0, QRTR_PORT_CTRL, 0, &p, sizeof(p));
	p = server_pkt(QRTR_TYPE_DEL_SERVER, 69, 1, 0, 0x2a);
	v1("del_server_from_modem", QRTR_TYPE_DEL_SERVER, 0, QRTR_PORT_CTRL, 1, QRTR_PORT_CTRL, 0, &p, sizeof(p));
	p = server_pkt(QRTR_TYPE_DEL_SERVER, 69, 1, 1, 0x4001);
	v1("del_server_announce_local", QRTR_TYPE_DEL_SERVER, 1, QRTR_PORT_CTRL, 0, QRTR_PORT_CTRL, 0, &p, sizeof(p));
	p = client_pkt(QRTR_TYPE_DEL_CLIENT, 1, 0x4001);
	v1("del_client_local_port_closed", QRTR_TYPE_DEL_CLIENT, 1, 0x4001, 0, QRTR_PORT_CTRL, 0, &p, sizeof(p));
	p = client_pkt(QRTR_TYPE_DEL_CLIENT, 0, 0x2a);
	v1("del_client_from_modem", QRTR_TYPE_DEL_CLIENT, 0, 0x2a, 1, QRTR_PORT_CTRL, 0, &p, sizeof(p));
	p = client_pkt(QRTR_TYPE_BYE, 0, 0);
	v1("bye_from_modem", QRTR_TYPE_BYE, 0, QRTR_PORT_CTRL, 1, QRTR_PORT_CTRL, 0, &p, sizeof(p));
	p = client_pkt(QRTR_TYPE_RESUME_TX, 1, 0x4001);
	v1("resume_tx_to_modem", QRTR_TYPE_RESUME_TX, 1, 0x4001, 0, 0x2a, 0, &p, sizeof(p));
	p = client_pkt(QRTR_TYPE_RESUME_TX, 0, 0x2a);
	v1("resume_tx_from_modem", QRTR_TYPE_RESUME_TX, 0, 0x2a, 1, 0x4001, 0, &p, sizeof(p));
	p = server_pkt(QRTR_TYPE_NEW_LOOKUP, 69, 1, 0, 0);
	v1("new_lookup_from_modem", QRTR_TYPE_NEW_LOOKUP, 0, 0x30, 1, QRTR_PORT_CTRL, 0, &p, sizeof(p));
	p = server_pkt(QRTR_TYPE_DEL_LOOKUP, 69, 1, 0, 0);
	v1("del_lookup_from_modem", QRTR_TYPE_DEL_LOOKUP, 0, 0x30, 1, QRTR_PORT_CTRL, 0, &p, sizeof(p));
	p = server_pkt(QRTR_TYPE_PING, 0, 0, 0, 0);
	v1("ping_from_modem", QRTR_TYPE_PING, 0, QRTR_PORT_CTRL, 1, QRTR_PORT_CTRL, 0, &p, sizeof(p));

	/* data: padding to four bytes, confirm_rx */
	v1("data_len1", QRTR_TYPE_DATA, 1, 0x4001, 0, 0x2a, 0, "a", 1);
	v1("data_len4", QRTR_TYPE_DATA, 1, 0x4001, 0, 0x2a, 0, "abcd", 4);
	v1("data_len5_confirm_rx", QRTR_TYPE_DATA, 1, 0x4001, 0, 0x2a, 1, "hello", 5);
	v1("data_len7", QRTR_TYPE_DATA, 0, 0x2a, 1, 0x4001, 0, "7 bytes", 7);
	v1("data_len8_from_modem_confirm_rx", QRTR_TYPE_DATA, 0, 0x2a, 1, 0x4001, 1, "8 bytes!", 8);
	v1("data_qmi_sized", QRTR_TYPE_DATA, 1, 0x4001, 0, 0x2a, 0,
	   "\x00\x34\x12\x24\x00\x07\x00\x02\x04\x00\x00\x00\x00\x00", 14);
	/* what the receiver must refuse */
	v1("data_len0", QRTR_TYPE_DATA, 1, 0x4001, 0, 0x2a, 0, "", 0);
	p = server_pkt(QRTR_TYPE_HELLO, 0, 0, 0, 0);
	v1("hello_dst_legacy_ctrl_port", QRTR_TYPE_HELLO, 0, QRTR_PORT_CTRL, 1, QRTR_PORT_CTRL_LEGACY, 0, &p, sizeof(p));
	v1("hello_type_word_0x102", 0x102, 0, QRTR_PORT_CTRL, 1, QRTR_PORT_CTRL, 0, &p, sizeof(p));
	v1("hello_to_data_port", QRTR_TYPE_HELLO, 0, QRTR_PORT_CTRL, 1, 0x4001, 0, &p, sizeof(p));
	v1("new_server_short_payload", QRTR_TYPE_NEW_SERVER, 0, QRTR_PORT_CTRL, 1, QRTR_PORT_CTRL, 0, &p, 16);
	v1("resume_tx_short_payload", QRTR_TYPE_RESUME_TX, 0, 0x2a, 1, 0x4001, 0, &p, 12);

	/* the second header layout */
	{
		static const u8 opt[4] = { 0xaa, 0xbb, 0xcc, 0xdd };

		v2("v2_data", QRTR_TYPE_DATA, 0, NULL, 0, 0, 0x2a, 1, 0x4001, "six!!!", 6);
		v2("v2_data_confirm_rx", QRTR_TYPE_DATA, QRTR_FLAGS_CONFIRM_RX, NULL, 0, 0x0102, 0x0304, 0x0506, 0x0708, "abcd", 4);
		v2("v2_data_options", QRTR_TYPE_DATA, 0, opt, 4, 0, 0x2a, 1, 0x4001, "opts", 4);
		v2("v2_hello_ctrl_ports", QRTR_TYPE_HELLO, 0, NULL, 0, 0, (u16)QRTR_PORT_CTRL, 1, (u16)QRTR_PORT_CTRL, &p, sizeof(p));
		v2("v2_hello_legacy_ctrl", QRTR_TYPE_HELLO, 0, NULL, 0, 0, 0x10, 1, QRTR_PORT_CTRL_LEGACY, &p, sizeof(p));
	}

	fclose(pk);
	return 0;
}
