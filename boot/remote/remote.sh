#!/bin/sh
# Remote access, then what init was asked to run (scripts/remote-initramfs.sh). /init runs this from the kernel command
# line, "-- /etc/lucina/remote.sh <command>". lucina-sshd waits for a network address by itself; its log goes to the
# console. The server keeps running if the command ends.
# lucina-bootlog saves the kernel log to the boot stick, lucinaos/juno-log-<N>.txt, for the first five minutes, and the crashes
# the kernel kept in /sys/fs/pstore.
mkdir -p /sys
mount -t sysfs sysfs /sys 2> /dev/null
services() {
    # The one queue of semantic events (docs/research/juno-events.md): programs tell it what happened, lucina-devd reads it for
    # Lucina. First, so that the kernel's events since boot and the shell's first ones are kept. Started again if it dies.
    (while :; do /sbin/lucina-eventd; sleep 1; done) >> /tmp/lucina-eventd.log 2>&1 &
    # The event log (docs/research/juno-event-log.md): reads the saved crash log and the PMIC's reasons into /data once /data is
    # mounted, then records app crashes, links going down and kernel errors as they come. Started again if it ends.
    (while :; do /sbin/lucina-evlog follow; sleep 5; done) >> /tmp/lucina-evlog.log 2>&1 &
    /sbin/lucina-bootlog &
    # A generation on trial, and the lines after a rollback or a crash.
    /sbin/lucina-trial > /run/lucina/trial.log 2>&1 &
    # lucina-watchd starts lucina-sshd and restarts it if it dies, pets /dev/watchdog and reboots a machine whose network stayed dead.
    /sbin/lucina-watchd &
    # The wall clock over the network: this kernel starts in 1970.
    /sbin/lucina-timesync > /tmp/lucina-timesync.log 2>&1 &
    # The sign-in to the owner's account, for every program on this machine, kept on /data so it survives a restart (the owner's
    # decision dc-4e82669418a1; docs/research/juno-account.md says what that exposes). Started again if it dies.
    (while :; do /sbin/lucina-accountd --keep-on-disk; sleep 1; done) >> /tmp/lucina-accountd.log 2>&1 &
    # Lucina's own tools on this laptop (docs/research/juno-lucina-tools.md): it dials out on the sign-in above and long-polls for
    # her calls. No port listens. Started again if it ends.
    (while :; do /sbin/lucina-devd; sleep 2; done) >> /tmp/lucina-devd.log 2>&1 &
    # The home network, when the bring-up is on and the credentials are in the initramfs or on the EFI partition.
    if grep -Eq 'lucina\.wifi=(on|force)' /proc/cmdline; then
        /sbin/lucina-wifi connect --saved &
    fi
}
if [ -e /run/lucina/data-expected ]; then
    # From the root partition every one of these pages in from the disk on the one CPU: after the shell's first frame, or 3 s
    # from now if none comes, so that a shell that cannot start leaves SSH working.
    (
        n=0
        until [ -e /run/lucina/first-frame ] || [ "$n" -ge 150 ]; do
            sleep 0.02
            n=$((n + 1))
        done
        services
        wait
    ) &
else
    services
fi
if [ $# -gt 0 ]; then
    "$@"
    echo "remote.sh: $1 ended (status $?): the SSH server stays up"
fi
wait
