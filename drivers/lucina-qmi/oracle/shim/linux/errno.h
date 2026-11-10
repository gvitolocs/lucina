/* glibc's <errno.h> pulls <linux/errno.h> for the numbers; give it the generic ones. */
#include <asm-generic/errno.h>
#include "kshim.h"
