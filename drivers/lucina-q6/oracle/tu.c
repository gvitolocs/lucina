/* One of Linux's files, unchanged, and its driver under a name main.c can reach. Built once per file: ORACLE_FILE and ORACLE_DRIVER. */
#define STR2(x) #x
#define STR(x) STR2(x)
#include STR(ORACLE_FILE)
struct apr_driver *ORACLE_EXPORT = &ORACLE_DRIVER;
#ifdef ORACLE_AFE_LANES
/* q6afe_cdc_dma_port_prepare fills active_channels_mask only when it is given 0 and never copies a mask it is given, so Linux cannot send
 * one. Windows' ADCM writes the field of the same payload itself (qcadcm7180.sys (address not published)); this does that, and nothing else. */
void oracle_set_cdc_dma_lanes(struct q6afe_port *port, u16 mask)
{
	port->port_cfg.dma_cfg.active_channels_mask = mask;
}
#endif
