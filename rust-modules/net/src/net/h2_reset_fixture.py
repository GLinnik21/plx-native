"""One synthetic TLS/H2 reset using stdlib + openssl; never contacts an external service.

Frames follow RFC 9113 sections 6.1/6.2/6.4/6.5; the literal :status uses RFC 7541's
static name index 8, without Huffman coding or dynamic-table state. This is a deliberately
tiny test peer, not a general HTTP/2 server. Its ephemeral certificate is trusted only by
the test request's existing CaBundle mode, never installed in the host trust store.
"""
import json
import os
import socket
import ssl
import subprocess
import sys
import tempfile
import time


def frame(kind, flags, stream, payload=b""):
    return len(payload).to_bytes(3, "big") + bytes((kind, flags)) + stream.to_bytes(4, "big") + payload


def receive(sock, size):
    buf = bytearray()
    while len(buf) < size:
        part = sock.recv(size - len(buf))
        if not part:
            raise RuntimeError("client closed before request headers")
        buf.extend(part)
    return bytes(buf)


def main():
    status = int(sys.argv[1])
    assert status in (401, 403, 404, 410)
    with tempfile.TemporaryDirectory(prefix="plx-h2-reset-") as tmp:
        cert = os.path.join(tmp, "cert.pem")
        key = os.path.join(tmp, "key.pem")
        config = os.path.join(tmp, "openssl.cnf")
        with open(config, "w", encoding="ascii") as output:
            output.write("[req]\nprompt=no\ndistinguished_name=dn\nx509_extensions=ext\n"
                         "[dn]\nCN=synthetic-loopback\n[ext]\nsubjectAltName=IP:127.0.0.1\n"
                         "basicConstraints=critical,CA:TRUE\n")
        subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
                        "-days", "1", "-config", config, "-keyout", key, "-out", cert],
                       check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                       timeout=15)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(cert, key)
        context.set_alpn_protocols(["h2"])
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            listener.listen(1)
            listener.settimeout(10)
            print(json.dumps({"port": listener.getsockname()[1], "ca": cert}), flush=True)
            raw, _ = listener.accept()
            with context.wrap_socket(raw, server_side=True) as peer:
                peer.settimeout(5)
                assert peer.selected_alpn_protocol() == "h2", "client did not negotiate HTTP/2"
                assert receive(peer, 24) == b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n"
                peer.sendall(frame(4, 0, 0))  # server SETTINGS
                while True:
                    header = receive(peer, 9)
                    length = int.from_bytes(header[:3], "big")
                    assert length <= 16384
                    payload = receive(peer, length)
                    kind, flags = header[3], header[4]
                    stream = int.from_bytes(header[5:], "big") & 0x7fffffff
                    if kind == 4 and not flags & 1:
                        peer.sendall(frame(4, 1, 0))  # SETTINGS acknowledgement
                    if kind == 1 and stream:
                        assert flags & 4, "fixture expects a single request HEADERS block"
                        # Literal without indexing, indexed :status name, three ASCII digits.
                        peer.sendall(frame(1, 4, stream, b"\x08\x03" + str(status).encode("ascii")))
                        peer.sendall(frame(0, 0, stream, b"short"))  # no END_STREAM
                        time.sleep(0.05)
                        peer.sendall(frame(3, 0, stream, (2).to_bytes(4, "big")))  # INTERNAL_ERROR
                        print("h2-reset-sent", flush=True)
                        break
            # Rust acknowledges after perform returns, keeping the temporary trust file alive.
            sys.stdin.readline()


if __name__ == "__main__":
    main()
