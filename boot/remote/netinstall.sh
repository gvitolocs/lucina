#!/bin/sh
# The net installer of the stick (docs/design/lucina-net-installer.md sections 2 and 3). /init runs it from the kernel
# command line, "-- /bin/sh /etc/lucina/netinstall.sh" (scripts/lucina-net-stick.sh writes that line, and
# scripts/juno-rescue-initramfs.sh --netinstall puts this file in the initramfs).
#
# What happens here is on purpose very little: name the machine, find the stick, wait for the wire, and hand over. The
# download, the checks and every write are lucina-install-disk's netinstall mode, and it is the one that refuses an
# unknown board and a machine with no network. Nothing on this machine's disk is touched by these lines.
#
# It runs from /bin/sh in the rescue initramfs (BusyBox), with the PATH the kernel command line sets.

# The stick's FAT volume, mounted read-only here: the installer writes the machine's disk, never the stick.
STICK=/mnt/lucina-stick
# How long to wait for a link (docs section 3): the owner plugs the adapter in after he reads the screen. A proof run that has
# no network on purpose says `lucina.netinst.wait=<seconds>` on the kernel command line; a stick never does.
LINK_WAIT=600

log() { echo "netinstall: $*"; }

# No screen scrollback and no SSH here: what is said is what stays on the console. A shell at the end keeps it readable.
give_up() {
    log "$*"
    log "nothing was downloaded and nothing was written"
    exec sh -l
}

mkdir -p /proc /sys "$STICK"
mount -t proc proc /proc 2> /dev/null
mount -t sysfs sysfs /sys 2> /dev/null

wait_word=$(sed -n 's/.*lucina\.netinst\.wait=\([0-9][0-9]*\)\( .*\)\{0,1\}$/\1/p' /proc/cmdline)
if [ -n "$wait_word" ]; then
    LINK_WAIT=$wait_word
fi

# The board the loader named (its own detection, docs section 2). The last word on the line counts, as in lucina-init.
board=$(sed -n 's/.*lucinaos.board=\([^ ]*\).*/\1/p' /proc/cmdline)
if [ -z "$board" ]; then
    give_up "the loader said no board: it does not know this machine. Its screen has the two lines to add to \\LUCINA\\BOARDS and to the compatible table"
fi

# The stick: the USB FAT volume holding LUCINA/. Partitions first, then a stick whose FAT sits on the whole disk. The kernel
# registers the stick's disk a second or more after this script starts (the first QEMU proof found /dev/sda 0.2 s too late), so
# the scan repeats for up to a minute.
stick_dev=
stick_wait=0
while [ -z "$stick_dev" ] && [ "$stick_wait" -lt 60 ]; do
    for dev in /dev/sd[a-z]*[0-9] /dev/sd[a-z]; do
        [ -b "$dev" ] || continue
        mount -t vfat -o ro "$dev" "$STICK" 2> /dev/null || continue
        if [ -f "$STICK/LUCINA/SERVER" ] && [ -f "$STICK/LUCINA/KEYS" ]; then
            stick_dev=$dev
            break
        fi
        umount "$STICK" 2> /dev/null
    done
    if [ -z "$stick_dev" ]; then
        sleep 1
        stick_wait=$((stick_wait + 1))
    fi
done
if [ -z "$stick_dev" ]; then
    give_up "no USB stick with LUCINA/SERVER and LUCINA/KEYS was mounted"
fi
server=$(sed -n '1p' "$STICK/LUCINA/SERVER" | tr -d '\r\n ')
log "stick on $stick_dev, board $board, server $server"
if [ -z "$server" ]; then
    give_up "$STICK/LUCINA/SERVER holds no address"
fi

# USB Ethernet needs no firmware and no credentials, so it comes first (lucina.rtl8152=on is on the command line).
# The kernel does the rest by itself (patch 0203): every network device asks for an address with DHCP, and only an answered
# one becomes an interface with that address. This kernel has no /sys/class/net, so there is no carrier file to read (found by
# the QEMU proof, scripts/qemu-net-install.sh): a non-loopback interface that holds an IPv4 address is the link. Which name
# the adapter gets is the kernel's, not ours.
n=0
link=
while [ -z "$link" ]; do
    link=$(ip -4 addr 2> /dev/null | sed -n 's/^ *inet \([0-9.]*\)\/[0-9]* .* \([^ ]*\)$/\2 \1/p' | grep -v '^lo ' | head -n 1)
    if [ -n "$link" ]; then
        break
    fi
    if [ "$n" -ge "$LINK_WAIT" ]; then
        give_up "no network link after ${LINK_WAIT}s: plug in a USB Ethernet adapter and try again"
    fi
    sleep 2
    n=$((n + 2))
done
log "link: ${link%% *} has ${link##* } after ${n}s"

# The address, the manifest, the chunks and the write: all of it is the installer's, and it checks what it writes before
# it writes it. Its own words go to this console.
# The kernel goes to the same RAM address as on the stick, so the stick's own BOOT.CFG names it (kernel = <file> <address>).
base=$(sed -n 's/^kernel *= *[^ ]* *\(0x[0-9a-fA-F][0-9a-fA-F]*\).*/\1/p' "$STICK/LUCINA/BOOT.CFG" | head -n 1)
if [ -z "$base" ]; then
    give_up "$STICK/LUCINA/BOOT.CFG names no kernel address"
fi
/sbin/lucina-install-disk netinstall --server "$server" --keys "$STICK/LUCINA/KEYS" --board "$board" --base "$base"
rc=$?

if [ "$rc" = 0 ]; then
    log "done: $board is written. The first boot is a trial; watch the screen before you touch anything."
else
    log "lucina-install-disk said no (status $rc): nothing was made bootable"
fi
exec sh -l
