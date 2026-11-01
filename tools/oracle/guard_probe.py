#!/usr/bin/env python3
"""Regression probe for the oracle safety-guard allow/deny matrix.

Usage (from the repository root):

    uv run python tools/oracle/guard_probe.py

Exits 0 when every guarded operation is refused and every disclosed local
operation still succeeds, and 1 on the first leak or break. This pins the
policy of ``support.install_safety_guards``: name resolution is refused for
None, empty, wildcard, and nonnumeric hosts while non-wildcard numeric
``getaddrinfo`` and ``AF_NETLINK`` create/bind stay permitted. All denied
probes target nonexistent canary paths so the leak path can never touch real
secret state.
"""

from __future__ import annotations

import socket
import sys
import tempfile
import urllib.request
from collections.abc import Callable
from pathlib import Path

from support import (
    CORPUS_DIGEST,
    OracleSafetyError,
    disk_corpus_digest,
    install_safety_guards,
)

_NETLINK = getattr(socket, "AF_NETLINK", 16)


def _read_first_byte(path: str) -> int:
    with Path(path).open("rb") as handle:
        data = handle.read(1)
    return data[0]


def main() -> int:
    """Run the guard matrix and report every deviation."""
    repo_root = Path(__file__).resolve().parents[2]
    temporary_directory = tempfile.TemporaryDirectory(prefix="oracle-guard-")
    temporary_root = Path(temporary_directory.name)
    safe_file = temporary_root / "safe.bin"
    safe_file.write_bytes(b"safe")
    e2e_link = temporary_root / "iptv-e2e-synthetic.env"
    e2e_link.symlink_to(safe_file)
    capture_link = temporary_root / "synthetic.pcap"
    capture_link.symlink_to(safe_file)
    var_dir = repo_root / "var"
    var_was_present = var_dir.exists()
    var_dir.mkdir(exist_ok=True)
    var_link = var_dir / "synthetic-link"
    var_link.symlink_to(safe_file)
    resolved_link = temporary_root / "resolved-into-var"
    resolved_link.symlink_to(var_dir / "synthetic-target")
    install_safety_guards(repo_root)
    failures: list[str] = []

    def expect_denied(label: str, run: Callable[[], object]) -> None:
        try:
            run()
        except OracleSafetyError:
            print(f"DENIED    {label}")
        except OSError as error:
            failures.append(f"LEAKED    {label}: {error}")
        else:
            failures.append(f"LEAKED    {label}")

    def expect_permitted(label: str, run: Callable[[], object]) -> None:
        try:
            run()
        except OracleSafetyError as error:
            failures.append(f"BROKEN    {label}: {error}")
        else:
            print(f"PERMITTED {label}")

    netlink = socket.socket(_NETLINK, socket.SOCK_RAW)

    expect_denied("var/ read", lambda: _read_first_byte("var/probe-canary"))
    original_resolve = Path.resolve

    def fail_resolve(self: Path, strict: bool = False) -> Path:
        del self, strict
        raise OSError("forced resolve failure")

    Path.resolve = fail_resolve
    expect_denied(
        "var/ lexical guard before resolve",
        lambda: _read_first_byte("var/resolve-failure-canary"),
    )
    Path.resolve = original_resolve
    expect_denied("var/ lexical symlink read", lambda: _read_first_byte(str(var_link)))
    expect_denied(
        "resolved symlink into var/", lambda: _read_first_byte(str(resolved_link))
    )
    expect_denied(
        "E2E env name read", lambda: _read_first_byte("var/iptv-e2e-canary.env")
    )
    expect_denied("E2E lexical symlink read", lambda: _read_first_byte(str(e2e_link)))
    expect_denied("capture read", lambda: _read_first_byte("/tmp/probe-capture.pcap"))
    expect_denied(
        "capture lexical symlink read", lambda: _read_first_byte(str(capture_link))
    )
    expect_denied(
        "AF_INET socket creation",
        lambda: socket.socket(socket.AF_INET, socket.SOCK_DGRAM),
    )
    expect_denied(
        "AF_INET6 socket creation",
        lambda: socket.socket(socket.AF_INET6, socket.SOCK_STREAM),
    )
    expect_denied("hostname resolution", lambda: socket.getaddrinfo("example.com", 0))
    expect_denied("None resolution", lambda: socket.getaddrinfo(None, 0))
    expect_denied("empty resolution", lambda: socket.getaddrinfo("", 0))
    expect_denied(
        "wildcard literal resolution", lambda: socket.getaddrinfo("0.0.0.0", 0)
    )
    expect_denied(
        "IPv6 wildcard literal resolution", lambda: socket.getaddrinfo("::", 0)
    )
    expect_denied(
        "DNS helper gethostbyname", lambda: socket.gethostbyname("example.com")
    )
    expect_denied("reverse lookup", lambda: socket.gethostbyaddr("192.168.1.128"))
    expect_denied(
        "URL egress",
        lambda: urllib.request.urlopen("http://192.0.2.1/probe", timeout=1),
    )
    expect_denied("connect", lambda: netlink.connect((0, 0)))
    expect_denied("sendto", lambda: netlink.sendto(b"\0", (0, 0)))

    expect_permitted(
        "numeric getaddrinfo", lambda: socket.getaddrinfo("192.168.1.128", 0)
    )
    expect_permitted(
        "numeric getaddrinfo (bytes literal)",
        lambda: socket.getaddrinfo(b"192.168.1.128", 0),
    )
    expect_permitted(
        "AF_NETLINK create", lambda: socket.socket(_NETLINK, socket.SOCK_RAW)
    )
    expect_permitted("AF_NETLINK bind", lambda: netlink.bind((0, 0)))

    digest = disk_corpus_digest(repo_root / "rust/tests/fixtures")
    if digest != CORPUS_DIGEST:
        failures.append(f"CORPUS_DIGEST_MISMATCH {digest}")
    else:
        print(f"CORPUS_DIGEST={digest}")

    for line in failures:
        print(line, file=sys.stderr)
    print(f"GUARD_PROBE_{'FAILED' if failures else 'OK'} ({len(failures)} deviations)")
    var_link.unlink(missing_ok=True)
    if not var_was_present:
        var_dir.rmdir()
    temporary_directory.cleanup()
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
