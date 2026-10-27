#!/usr/bin/env python3
"""Static file server that drops the first N requests for each file: the
connection is closed with no reply, like a download cut off mid-way. Prints one
line per request, e.g. `GET /version #1 dropped`. Used by TC-112.
  flaky-http.py <dir> <port> <drops per file>"""
import functools
import http.server
import socket
import sys
from collections import Counter

root, port, drops = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
seen = Counter()


class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        seen[self.path] += 1
        n = seen[self.path]
        if n <= drops:
            print(f"GET {self.path} #{n} dropped", flush=True)
            self.close_connection = True
            self.connection.shutdown(socket.SHUT_RDWR)
            return
        print(f"GET {self.path} #{n} served", flush=True)
        super().do_GET()

    def log_message(self, *args):
        pass


handler = functools.partial(Handler, directory=root)
http.server.ThreadingHTTPServer(("127.0.0.1", port), handler).serve_forever()
