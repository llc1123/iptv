"""Deterministic inputs, safety guards, and fixture shape for the oracle generator.

Every value produced by the fixture generator originates from the synthetic
constants in this module or from deterministic execution of the Python POC
modules in ``iptv_client``. No runtime secrets, environment values, host
paths, or wall-clock times take part in generation.
"""

from __future__ import annotations

import ipaddress
import json
import os
import socket
import sys
import time
from dataclasses import dataclass
from hashlib import sha256
from pathlib import Path
from typing import TYPE_CHECKING, Final

if TYPE_CHECKING:
    from iptv_client.models import BootChannel, Programme

SCHEMA: Final = "iptv-poc-golden"
SCHEMA_VERSION: Final = 1
CORPUS_DIGEST: Final = (
    "9e6f1a04534608d92c44e66a59b40c143f776e93f9e56346195abddb6616011c"
)

# Synthetic fixture inputs. These are deliberately fake and documented so a
# reviewer can prove that no runtime credential can enter the fixture corpus.
FIXTURE_DHCP_USER: Final = "fixture-user"
FIXTURE_DHCP_PASSWORD: Final = "fixture-dhcp-password"
FIXTURE_EPG_USER: Final = "fixture-user"
FIXTURE_EPG_PASSWORD: Final = "fixture-epg-password"
FIXTURE_ENCRYPT_TOKEN: Final = "FIXTURE-ENCRYPT-TOKEN"
FIXTURE_STB_ID: Final = "0102030405060708"
FIXTURE_IP_ADDRESS: Final = "192.0.2.10"
FIXTURE_MAC: Final = "02:00:00:00:00:01"
FIXTURE_CUSTOM_STR: Final = "$CTC"
FIXTURE_RANDOM_VALUE: Final = 12_345_678
FIXTURE_R_HEX: Final = "7856341200000000"  # 0x12345678 little-endian, 31-bit r
FIXTURE_TS_HEX: Final = "cb047b4514000000"  # 1_234_567_890_123 little-endian ms
FIXTURE_BASE_URL: Final = "http://192.0.2.1:8080"
FIXTURE_UPSTREAM_BASE: Final = "http://upstream.example.com:8080/live/chan.m3u8"

# Fixed deployment timezone used for XMLTV rendering and for interpreting
# naive ISO catch-up values. POSIX TZ form forces a fixed +08:00 offset with
# no daylight-saving transitions.
TIMEZONE_POSIX: Final = "<+08>-8"
RENDER_OFFSET_MINUTES: Final = 480

REQUIRED_COVERAGE: Final = (
    "aligned-padding",
    "non-aligned-padding",
    "malformed-lengths",
    "duplicate-attributes",
    "html-entities",
    "crlf-playlists",
    "cr-playlists",
    "bom-playlists",
    "relative-urls",
    "protocol-relative-urls",
    "query-urls",
    "xml-escaping",
    "numeric-catchup",
    "iso-catchup",
    "failure-status-shapes",
)

ERROR_KINDS: Final = (
    "option60-oversized-payload",
    "option60-malformed-prefix",
    "option60-malformed-length",
    "option60-malformed-method",
    "parsers-no-boot-channels",
    "models-validation",
    "catchup-missing-time",
    "catchup-invalid-time",
    "guide-cache-not-a-list",
    "guide-cache-invalid-json",
    "guide-cache-invalid-programme",
    "hls-resource-expired",
    "provider-status-failure",
)

FORBIDDEN_MARKERS: Final = (
    "iptv-e2e",
    "/root/",
    "var/",
    "IPTV_PASSWORD",
    "IPTV_DHCP_PASSWORD",
    "IPTV_USER_ID",
    "IPTV_DHCP_USER",
)

_CAPTURE_SUFFIXES: Final = (".pcap", ".pcapng", ".har", ".saz", ".qlog")


class OracleSafetyError(RuntimeError):
    """Raised when generation would touch network or runtime-secret sources."""


def install_safety_guards(repo_root: Path) -> None:
    """Refuse secret-state access and every egress-capable socket operation.

    The audit hook cannot be removed once installed. It denies reading runtime
    secret state (anything under ``var/``, any path named like the gitignored
    E2E environment file, capture archives) and everything that could put a
    byte on the wire: non-``AF_NETLINK`` socket creation, non-``AF_NETLINK``
    binds, connect/send events, and name resolution of any host that is not a
    non-wildcard numeric literal (None, empty, wildcard, and nonnumeric hosts
    are refused). The only permitted socket traffic is scapy's import-time
    kernel interface enumeration (``AF_NETLINK`` create/bind) and non-wildcard
    numeric ``getaddrinfo``: both kernel- and libc-local, incapable of network
    egress. The allow/deny matrix is pinned by ``tools/oracle/guard_probe.py``.
    """
    repo_root = repo_root.resolve()
    denied_dir = repo_root / "var"
    netlink = getattr(socket, "AF_NETLINK", 16)

    def is_numeric_host(raw: object) -> bool:
        """Return whether ``raw`` is an explicit non-wildcard numeric literal.

        None, empty, wildcard, and nonnumeric hosts are refused so only
        literal resolution (kernel-local, no DNS, no wildcard synthesis) can
        pass the guard.
        """
        if not isinstance(raw, (str, bytes)):
            return False
        try:
            address = ipaddress.ip_address(os.fsdecode(raw))
        except ValueError:
            return False
        return not address.is_unspecified

    def is_forbidden_path(path: Path) -> bool:
        return (
            path == denied_dir
            or denied_dir in path.parents
            or any("iptv-e2e" in part for part in path.parts)
            or path.suffix.lower() in _CAPTURE_SUFFIXES
        )

    def hook(event: str, args: tuple[object, ...]) -> None:
        if event in {
            "socket.connect",
            "socket.connect_ex",
            "socket.gethostbyaddr",
            "socket.gethostbyname",
            "socket.getnameinfo",
            "socket.sendmsg",
            "socket.sendto",
            "urllib.Request",
        }:
            raise OracleSafetyError(
                f"network access is forbidden in generation: {event}"
            )
        if event == "socket.getaddrinfo":
            if not is_numeric_host(args[0] if args else None):
                raise OracleSafetyError(
                    f"name resolution is forbidden in generation: {event}"
                )
            return
        if event == "socket.__new__":
            if len(args) < 2 or args[1] != netlink:
                raise OracleSafetyError(
                    f"non-netlink socket creation is forbidden in generation: {event}"
                )
            return
        if event == "socket.bind":
            sock = args[0] if args else None
            if getattr(sock, "family", None) != netlink:
                raise OracleSafetyError(
                    f"non-netlink socket bind is forbidden in generation: {event}"
                )
            return
        if event != "open" or not args:
            return
        raw = args[0]
        if not isinstance(raw, (str, bytes, os.PathLike)):
            return
        requested = Path(os.fsdecode(raw))
        lexical_path = Path(os.path.abspath(os.fspath(requested)))
        if is_forbidden_path(lexical_path):
            raise OracleSafetyError(
                f"runtime secret state is forbidden in generation: {lexical_path}"
            )
        try:
            resolved_path = requested.resolve()
        except (OSError, ValueError):  # unresolvable descriptors are not files we deny
            return
        if is_forbidden_path(resolved_path):
            raise OracleSafetyError(
                f"runtime secret state is forbidden in generation: {resolved_path}"
            )

    sys.addaudithook(hook)


def force_timezone() -> None:
    """Pin local time so naive ISO catch-up values stay deterministic."""
    os.environ["TZ"] = TIMEZONE_POSIX
    time.tzset()


def json_text(value: object) -> str:
    """Render fixture JSON with byte-stable formatting."""
    return json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n"


def sha256_hex(data: bytes) -> str:
    """Return the lowercase SHA-256 hex digest of ``data``."""
    return sha256(data).hexdigest()


def corpus_digest(corpus: dict[str, bytes], relative_dir: Path) -> str:
    """Return the historical sorted-``sha256sum`` digest for generated files."""
    records = sorted(
        f"{sha256_hex(payload)}  {(relative_dir / name).as_posix()}"
        for name, payload in corpus.items()
    )
    return sha256_hex(("\n".join(records) + "\n").encode("utf-8"))


def disk_corpus_digest(output_dir: Path) -> str:
    """Return the sorted-``sha256sum`` digest for on-disk JSON fixtures."""
    corpus = {
        path.relative_to(output_dir).as_posix(): path.read_bytes()
        for path in sorted(output_dir.glob("*.json"))
    }
    return corpus_digest(corpus, Path("rust/tests/fixtures"))


@dataclass(frozen=True, slots=True)
class Case:
    """One golden fixture case: deterministic input plus expected outcome."""

    id: str
    covers: tuple[str, ...]
    input: dict[str, object]
    expected: dict[str, object]

    def to_json(self) -> dict[str, object]:
        """Render the case as its JSON object form."""
        return {
            "id": self.id,
            "covers": list(self.covers),
            "input": self.input,
            "expected": self.expected,
        }


def error_expected(
    kind: str, *, operation: str, detail: str | None = None
) -> dict[str, object]:
    """Build an ``expected`` object for a typed failure case."""
    if kind not in ERROR_KINDS:
        raise OracleSafetyError(f"error kind is not in the closed vocabulary: {kind}")
    error: dict[str, object] = {"kind": kind, "operation": operation}
    if detail is not None:
        error["detail"] = detail
    return {"error": error}


class FakeSecrets:
    """Deterministic stand-in for ``secrets.token_urlsafe``."""

    def __init__(self) -> None:
        self._counter = 0

    def reset(self) -> None:
        """Restart the token sequence for the next fixture case."""
        self._counter = 0

    def token_urlsafe(self, nbytes: int) -> str:
        """Return the next fixed 24-character URL-safe token."""
        if nbytes != 18:
            raise OracleSafetyError(f"unexpected token size from the oracle: {nbytes}")
        self._counter += 1
        return f"fixturetok{self._counter:014d}"


class FakeClock:
    """Deterministic stand-in for ``time.monotonic`` inside the HLS proxy."""

    def __init__(self, start_ms: int = 0) -> None:
        self.now_ms = start_ms

    def advance_to(self, now_ms: int) -> None:
        """Move the injected monotonic clock to ``now_ms``."""
        self.now_ms = now_ms

    def monotonic(self) -> float:
        """Return the injected monotonic reading in seconds."""
        return self.now_ms / 1000.0


def make_channel(payload: dict[str, object]) -> BootChannel:
    """Validate one provider channel payload through the POC model."""
    # Imported lazily so no POC module loads before generate.py installs the
    # safety guard (iptv_client.dhcp reads environment at import time).
    from iptv_client.models import BootChannel

    return BootChannel.model_validate(payload)


def make_programme(payload: dict[str, object]) -> Programme:
    """Validate one provider programme payload through the POC model."""
    from iptv_client.models import Programme

    return Programme.model_validate(payload)
