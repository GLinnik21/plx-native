#!/usr/bin/env python3
"""Receiver for the app's network log (build.sh --log-host IPV4:PORT).

    python3 tizen/hello/log-receiver.py [PORT]        # default 9999, Ctrl-C to stop

Prints each datagram as it arrives and says so when the sequence numbers skip. The library sends
"<seq> <line>", the C# host "h<seq> <line>"; each stream counts from 000000 at every app start.
(`nc -klu PORT` shows the same lines without the gap check.)
"""
import re
import socket
import sys

port = int(sys.argv[1]) if len(sys.argv) > 1 else 9999
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.bind(("0.0.0.0", port))
print(f"listening on udp/{port}", file=sys.stderr, flush=True)

expect = {}  # (sender address, stream) -> next sequence number
while True:
    data, peer = sock.recvfrom(4096)
    text = data.decode("utf-8", "replace")
    m = re.match(r"(h?)(\d+) ", text)
    if m:
        key, seq = (peer[0], m.group(1)), int(m.group(2))
        want = expect.get(key, 0)
        if seq > want:
            print(f"-- {seq - want} datagram(s) lost before {m.group(1)}{seq:06d}", flush=True)
        elif seq < want:
            print(f"-- {m.group(1)}{seq:06d} after {m.group(1)}{want - 1:06d}: app restarted or reordered", flush=True)
        expect[key] = seq + 1
    print(text, end="" if text.endswith("\n") else "\n", flush=True)
