"""Exercise Wasmtime startup and the UI using the packaged server's final signature."""

from __future__ import annotations

import argparse
import base64
import http.client
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import time


def listening_port(pid: int) -> int | None:
    result = subprocess.run(
        ["/usr/sbin/lsof", "-nP", "-a", "-p", str(pid),
         "-iTCP", "-sTCP:LISTEN", "-Fn"],
        capture_output=True,
        text=True,
        timeout=5,
        check=False,
    )
    if result.returncode not in (0, 1) or result.stderr:
        raise RuntimeError(f"cannot inspect server listener: {result.stderr.strip()}")
    ports = [
        int(line.removeprefix("n127.0.0.1:"))
        for line in result.stdout.splitlines()
        if line.startswith("n127.0.0.1:")
    ]
    if len(ports) > 1:
        raise RuntimeError(f"server has multiple loopback listeners: {ports}")
    return ports[0] if ports else None


def ui_ready(port: int) -> bool:
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
    try:
        connection.request("GET", "/health")
        response = connection.getresponse()
        health = json.loads(response.read())
        if response.status != 200 or not isinstance(health, dict):
            raise RuntimeError(f"unexpected health response: HTTP {response.status}")
        if health.get("ready") is not True:
            return False
        connection.request("GET", "/")
        response = connection.getresponse()
        document = response.read().decode("utf-8")
        if response.status != 200 or not re.search(
            r"<title[^>]*>[^<]*scryer[^<]*</title>", document, re.IGNORECASE
        ):
            raise RuntimeError(f"packaged UI is missing: HTTP {response.status}")
        return True
    finally:
        connection.close()


def smoke_test(app: Path) -> None:
    server_binary = app.resolve() / "Contents" / "MacOS" / "scryer"
    subprocess.run(
        ["/usr/bin/codesign", "--verify", "--deep", "--strict", str(app.resolve())],
        check=True,
        timeout=30,
    )
    with tempfile.TemporaryDirectory(prefix="scryer-macos-smoke-") as directory:
        root = Path(directory)
        home = root / "home"
        home.mkdir()
        # Do not inherit operator credentials, dotenv files, caches, or Keychain access.
        environment = {
            "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
            "HOME": str(home),
            "TMPDIR": str(root),
            "SCRYER_BIND": "127.0.0.1:0",
            "SCRYER_OPEN_BROWSER": "false",
            "SCRYER_TRAY_SUPERVISED": "1",
            "SCRYER_DISABLE_PLATFORM_KEYSTORE": "true",
            "SCRYER_ENCRYPTION_KEY": base64.b64encode(os.urandom(32)).decode("ascii"),
            "SCRYER_METADATA_GATEWAY_GRAPHQL_URL": "http://127.0.0.1:0/graphql",
        }
        server = subprocess.Popen(
            [str(server_binary), "--data-dir", str(root / "data")],
            cwd=root,
            env=environment,
        )
        try:
            deadline = time.monotonic() + 120
            port = None
            last_error = "server has not opened its listener"
            while time.monotonic() < deadline:
                status = server.poll()
                if status is not None:
                    reason = signal.Signals(-status).name if status < 0 else str(status)
                    raise RuntimeError(f"packaged server exited before readiness: {reason}")
                # Discover this child's OS-assigned port rather than racing for a free port.
                if port is None:
                    port = listening_port(server.pid)
                if port is not None:
                    try:
                        if ui_ready(port):
                            if server.poll() is not None:
                                raise RuntimeError("packaged server exited during readiness check")
                            print("Packaged server ready: /health ready=true; Scryer UI HTTP 200",
                                  flush=True)
                            return
                        last_error = "/health is not ready"
                    except (ConnectionError, TimeoutError, http.client.RemoteDisconnected) as error:
                        last_error = f"{type(error).__name__}: {error}"
                try:
                    server.wait(timeout=0.25)
                except subprocess.TimeoutExpired:
                    pass
            raise RuntimeError(f"packaged startup timed out: {last_error}")
        finally:
            if server.poll() is None:
                server.terminate()
                try:
                    server.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait(timeout=10)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("app", type=Path, help="Final signed Scryer.app, including an extracted upgrade bundle.")
    args = parser.parse_args()
    try:
        smoke_test(args.app)
    except (OSError, ValueError, RuntimeError, http.client.HTTPException,
            subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
        print(f"macOS packaged startup failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
