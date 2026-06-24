#!/usr/bin/env python3
"""Extract evidence of QUIC Extended Key Update from the workbench's decrypted pcaps.

The `simulate` command writes one pcapng per node (`<node>.pcap`) with the TLS secrets
embedded as a Decryption Secrets Block, so Wireshark/tshark decrypt them directly.

This script reads the client and server endpoint pcaps and reports, from the *decrypted*
QUIC packets:

  * every `ExtendedKeyUpdate` post-handshake handshake message (subtype + KeyShareEntry),
    decoded byte-for-byte, and
  * the Key Phase bit on every 1-RTT packet, per direction, so the key-update toggles are
    visible.

Usage:
    # produce the pcaps (writes <node>.pcap in the working directory)
    cargo run --release -p quinn-workbench -- simulate \
        --network-graph test-data/earth-mars/networkgraph-fullmars-extended-key-update.json \
        --network-events test-data/earth-mars/events.json \
        --traffic test-data/earth-mars/request-response.traffic.json

    # analyze (pass the client and a server-endpoint pcap)
    python3 scripts/eku-pcap-evidence.py GND.pcap ING.pcap

Requires `tshark` (Wireshark) on PATH.
"""

import json
import re
import shutil
import subprocess
import sys

EKU_HANDSHAKE_TYPE = "1a"  # provisional codepoint for ExtendedKeyUpdate
SUBTYPES = {"00": "key_update_request", "01": "key_update_response", "02": "new_key_update"}
GROUPS = {"001d": "X25519", "0017": "secp256r1", "0018": "secp384r1"}


def tshark(*args):
    exe = shutil.which("tshark") or "/Applications/Wireshark.app/Contents/MacOS/tshark"
    return subprocess.run([exe, *args], capture_output=True, text=True).stdout


def find_all(obj, key, out):
    if isinstance(obj, dict):
        for k, v in obj.items():
            if k == key:
                out.append(v[0] if isinstance(v, list) else v)
            find_all(v, key, out)
    elif isinstance(obj, list):
        for x in obj:
            find_all(x, key, out)


def eku_messages(pcap):
    """Return [(frame, src, subtype_name, group_name, key_share_hex)] for each decrypted EKU message."""
    frames = json.loads(tshark("-r", pcap, "-T", "json", "-x"))
    msgs = []
    for fr in frames:
        layers = fr["_source"]["layers"]
        fnum = layers.get("frame", {}).get("frame.number")
        src = layers.get("ip", {}).get("ip.src", "")
        handshakes = []
        find_all(layers, "tls.handshake_raw", handshakes)
        for h in handshakes:
            if h[:2] != EKU_HANDSHAKE_TYPE:
                continue
            sub = SUBTYPES.get(h[8:10], f"unknown(0x{h[8:10]})")
            group = GROUPS.get(h[10:14], f"0x{h[10:14]}")
            key_len = int(h[14:18], 16)
            key = h[18 : 18 + key_len * 2]
            msgs.append((int(fnum), src, sub, group, key))
    return msgs


def key_phases(pcap):
    """Return {src_ip: 'KP-string'} of 1-RTT key-phase bits in capture order, per sender."""
    summary = tshark("-r", pcap)
    seq = {}
    for line in summary.splitlines():
        m = re.match(r"\s*\d+\s+[\d.]+\s+(\d+\.\d+\.\d+\.\d+)\s+→.*\(KP([01])\)", line)
        if m:
            seq.setdefault(m.group(1), []).append(m.group(2))
    return {ip: "".join(v) for ip, v in seq.items()}


def toggles(kp):
    return sum(1 for i in range(1, len(kp)) if kp[i] != kp[i - 1])


def main():
    pcaps = sys.argv[1:] or ["GND.pcap", "ING.pcap"]
    print("=== ExtendedKeyUpdate messages (decrypted from 1-RTT CRYPTO frames) ===\n")
    for pcap in pcaps:
        msgs = eku_messages(pcap)
        print(f"{pcap}:")
        if not msgs:
            print("  (none decrypted — note: only exchanges under the initial 1-RTT secret are")
            print("   decryptable, since Wireshark cannot derive the fresh-ECDHE update secrets)")
        for fnum, src, sub, group, key in msgs:
            print(f"  frame {fnum:>3} from {src:<14} {sub:<19} group={group} key_share={key}")
        print()

    print("=== Key Phase bit per 1-RTT packet, by sender (each toggle = one key update) ===\n")
    for pcap in pcaps:
        for ip, kp in sorted(key_phases(pcap).items()):
            print(f"  {pcap} {ip:<14} {kp}   -> {toggles(kp)} key updates")
        print()


if __name__ == "__main__":
    main()
