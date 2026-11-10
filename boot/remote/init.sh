#!/bin/sh
# PID 1 when the kernel starts from the root partition (`init=/etc/lucina/init.sh`, patch 0810). What /init does in an initramfs,
# for a root that stays read-only: everything that changes lives on tmpfs, or on /data. /data comes up in the background: nothing
# here waits for the disk beyond what the kernel already read, so the first frame does not either.
export PATH=/bin:/sbin:/usr/bin:/usr/sbin
export HOME=/data/home

mount -t sysfs none /sys
mount -t proc none /proc
for dir in /tmp /run /var /mnt; do
    mount -t tmpfs tmpfs "$dir"
done
chmod 1777 /tmp
mkdir -p /dev/pts /dev/shm /run/lucina
mount -t tmpfs tmpfs /dev/shm
chmod 1777 /dev/shm
mount -t devpts devpts /dev/pts
chmod 0666 /dev/pts/ptmx
ln -s pts/ptmx /dev/ptmx

# lucina-wifi and anything else that reads /data waits for data-ready or data-failed. Mounting takes the one CPU and the disk
# for hundreds of ms: after the shell's first frame, or 3 s from now if none comes.
: > /run/lucina/data-expected
(
    n=0
    until [ -e /run/lucina/first-frame ] || [ "$n" -ge 150 ]; do
        sleep 0.02
        n=$((n + 1))
    done
    exec /sbin/lucina-data mount > /run/lucina/data.out 2>&1
) &

# As /init does: `script` gives what follows a controlling terminal.
script /dev/null -q -c "$*"
echo "init.sh: $* ended"
exec sh -l
