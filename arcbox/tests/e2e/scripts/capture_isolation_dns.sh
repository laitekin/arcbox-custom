#!/bin/sh
set -eu
if [ "$#" -ne 1 ]; then
  echo "usage: sudo sh capture_isolation_dns.sh /absolute/path/dns.pcap" >&2
  exit 2
fi
capture_path=$1
capture_user=${SUDO_USER:-$(id -un)}
# Open BPF with privilege, then drop to the invoking user before saving.
# Match only the e2e's first DNS label prefix; ordinary DNS is excluded.
exec /usr/sbin/tcpdump -i pktap,all -n -U -s 0 -Z "$capture_user" \
  -w "$capture_path" 'udp port 53 and udp[21:4] = 0x6162786c'
