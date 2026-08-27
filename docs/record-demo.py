#!/usr/bin/env python3
"""Record the deterministic demo script as an asciicast v2 session."""

import errno
import fcntl
import json
import os
import pty
import select
import struct
import subprocess
import sys
import termios
import time


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: record-demo.py DEMO_SCRIPT OUTPUT_CAST", file=sys.stderr)
        return 2

    script = os.path.abspath(sys.argv[1])
    output = os.path.abspath(sys.argv[2])
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 118, 0, 0))
    env = os.environ.copy()
    env["TERM"] = "xterm-256color"
    process = subprocess.Popen(
        [script],
        stdin=subprocess.DEVNULL,
        stdout=slave,
        stderr=slave,
        cwd=os.path.dirname(os.path.dirname(script)),
        env=env,
        close_fds=True,
    )
    os.close(slave)
    started = time.monotonic()

    with open(output, "w", encoding="utf-8") as cast:
        cast.write(json.dumps({"version": 2, "width": 118, "height": 24}) + "\n")
        while True:
            ready, _, _ = select.select([master], [], [], 0.1)
            if master in ready:
                try:
                    data = os.read(master, 65536)
                except OSError as error:
                    if error.errno == errno.EIO:
                        break
                    raise
                if not data:
                    break
                event = [
                    round(time.monotonic() - started, 6),
                    "o",
                    data.decode("utf-8", errors="replace"),
                ]
                cast.write(json.dumps(event, ensure_ascii=False) + "\n")
                cast.flush()
            elif process.poll() is not None:
                break

    os.close(master)
    return process.wait()


if __name__ == "__main__":
    raise SystemExit(main())
