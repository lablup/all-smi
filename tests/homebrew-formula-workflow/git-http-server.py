#!/usr/bin/env python3
"""A single-repository, authenticating Git smart-HTTP server.

It exists so the push step of update_homebrew_formula.yml can be executed for
real. That step's entire claim is about where the tap credential goes: onto the
wire for one command, and nowhere else. Neither half is observable without a
server on the other end that records exactly what it was sent, so this one
demands HTTP Basic auth for every request and appends every Authorization
header it sees to a file before deciding anything.

Configuration comes from the environment:
  GIT_PROJECT_ROOT  directory holding the bare repository
  EXPECT_USER       username the push has to present
  EXPECT_PASS       password the push has to present
  AUTH_LOG          file to append every Authorization header to

Binds to an ephemeral port on the loopback interface, prints that port on
stdout, then serves until killed.
"""

import base64
import os
import subprocess
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PROJECT_ROOT = os.environ["GIT_PROJECT_ROOT"]
EXPECT_USER = os.environ["EXPECT_USER"]
EXPECT_PASS = os.environ["EXPECT_PASS"]
AUTH_LOG = os.environ["AUTH_LOG"]


class GitHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        """Silence the default stderr access log; the harness has its own."""

    def _record_and_check_auth(self) -> bool:
        header = self.headers.get("Authorization", "")
        with open(AUTH_LOG, "a", encoding="utf-8") as handle:
            handle.write(f"{self.command} {self.path} {header}\n")
        if not header.startswith("Basic "):
            return False
        try:
            decoded = base64.b64decode(header[len("Basic "):]).decode("utf-8")
        except (ValueError, UnicodeDecodeError):
            return False
        user, _, password = decoded.partition(":")
        return user == EXPECT_USER and password == EXPECT_PASS

    def _send(self, status: int, headers, body: bytes) -> None:
        self.send_response(status)
        for key, value in headers:
            self.send_header(key, value)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _read_body(self) -> bytes:
        if self.headers.get("Transfer-Encoding", "").lower() == "chunked":
            chunks = []
            while True:
                size = int(self.rfile.readline().split(b";")[0], 16)
                if size == 0:
                    self.rfile.readline()
                    break
                chunks.append(self.rfile.read(size))
                self.rfile.readline()
            return b"".join(chunks)
        length = int(self.headers.get("Content-Length", "0") or 0)
        return self.rfile.read(length) if length else b""

    def _serve(self) -> None:
        body = self._read_body()
        if not self._record_and_check_auth():
            self._send(401, [("WWW-Authenticate", 'Basic realm="git"')], b"")
            return

        path, _, query = self.path.partition("?")
        env = {
            "GIT_PROJECT_ROOT": PROJECT_ROOT,
            "GIT_HTTP_EXPORT_ALL": "1",
            "REQUEST_METHOD": self.command,
            "PATH_INFO": path,
            "QUERY_STRING": query,
            "REMOTE_USER": EXPECT_USER,
            "REMOTE_ADDR": self.client_address[0],
            "CONTENT_TYPE": self.headers.get("Content-Type", ""),
            "CONTENT_LENGTH": str(len(body)),
            "HTTP_CONTENT_ENCODING": self.headers.get("Content-Encoding", ""),
            "PATH": os.environ.get("PATH", ""),
            "HOME": os.environ.get("HOME", ""),
        }
        result = subprocess.run(
            ["git", "http-backend"], input=body, env=env, capture_output=True, check=False
        )
        if result.returncode != 0:
            self._send(500, [("Content-Type", "text/plain")], result.stderr)
            return

        raw = result.stdout
        head, separator, payload = raw.partition(b"\r\n\r\n")
        if not separator:
            head, separator, payload = raw.partition(b"\n\n")
        if not separator:
            head, payload = b"", raw

        status = 200
        headers = []
        for line in head.replace(b"\r\n", b"\n").split(b"\n"):
            if not line.strip():
                continue
            key, _, value = line.partition(b":")
            key_text = key.strip().decode("utf-8", "replace")
            value_text = value.strip().decode("utf-8", "replace")
            if key_text.lower() == "status":
                status = int(value_text.split()[0])
            else:
                headers.append((key_text, value_text))
        self._send(status, headers, payload)

    do_GET = _serve
    do_POST = _serve


def main() -> int:
    server = ThreadingHTTPServer(("127.0.0.1", 0), GitHandler)
    print(server.server_address[1], flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
