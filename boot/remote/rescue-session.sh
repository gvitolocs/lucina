#!/bin/sh
# The session of the rescue initramfs (scripts/juno-rescue-initramfs.sh). It runs when the root partition could not be used, or from
# the recovery stick. The screen comes back from a quiet boot, the kernel's own log says why we are here, and SSH is up if a
# cable is in.
/sbin/lucina-recover console 2> /dev/null
cat /etc/lucina/rescue-banner.txt
exec sh -l
