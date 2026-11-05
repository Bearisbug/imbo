#!/usr/bin/env python3
"""A slow network in front of the local server. Prints one line per
connection, e.g. `#1 POST /v1/messages HTTP/1.1 held 7s` or `#2 stalled`.
Used by TC-554.
  slow-net.py stall <port>
      accepts connections and never answers, like a TLS handshake that hangs
  slow-net.py delay <port> <server port> <seconds>
      passes each connection on to 127.0.0.1:<server port>, holding back the
      server's first reply for <seconds>"""
import socket
import sys
import threading
import time

mode, port = sys.argv[1], int(sys.argv[2])
count = 0


def drain(conn):
    with conn:
        while conn.recv(65536):
            pass


def pipe(src, dst, hold=0.0):
    try:
        first = True
        while data := src.recv(65536):
            if first and hold:
                time.sleep(hold)
            first = False
            dst.sendall(data)
    except OSError:
        pass
    finally:
        for s in (src, dst):
            try:
                s.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass


def relay(conn, n, upstream, hold):
    up = socket.create_connection(("127.0.0.1", upstream))
    first = conn.recv(65536)
    line = first.split(b"\r\n", 1)[0].decode("latin-1")
    print(f"#{n} {line} held {hold:g}s", flush=True)
    up.sendall(first)
    threading.Thread(target=pipe, args=(conn, up), daemon=True).start()
    pipe(up, conn, hold)
    conn.close()
    up.close()


server = socket.create_server(("127.0.0.1", port))
while True:
    conn, _ = server.accept()
    count += 1
    if mode == "stall":
        print(f"#{count} stalled", flush=True)
        threading.Thread(target=drain, args=(conn,), daemon=True).start()
    else:
        upstream, hold = int(sys.argv[3]), float(sys.argv[4])
        threading.Thread(
            target=relay, args=(conn, count, upstream, hold), daemon=True
        ).start()
