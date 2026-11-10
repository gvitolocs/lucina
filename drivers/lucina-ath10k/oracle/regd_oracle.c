/* Derived from Linux drivers/net/wireless/ath/regd_common.h and regd.h (SPDX ISC; Atheros Communications): the
 * regulatory pair table cut out unchanged, see oracle/README.md. */
#include <linux/types.h>
#include <stdio.h>
#include "regd_defs.h"
#include "regd_ctl.h"
#include "regd_enum.h"
#include "regd_pairs.h"
int main(void)
{
	size_t i;
	printf("const.NO_CTL %d\nconst.COUNTRY_ERD_FLAG %d\nconst.WORLDWIDE_ROAMING_FLAG %d\nconst.WOR4_WORLD %d\n", NO_CTL, COUNTRY_ERD_FLAG, WORLDWIDE_ROAMING_FLAG, WOR4_WORLD);
	for (i = 0; i < sizeof(regDomainPairs) / sizeof(regDomainPairs[0]); i++)
		printf("regpair 0x%x 0x%x 0x%x\n", regDomainPairs[i].reg_domain, regDomainPairs[i].reg_5ghz_ctl, regDomainPairs[i].reg_2ghz_ctl);
	return 0;
}
