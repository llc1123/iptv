#!/usr/bin/env python3
"""Regenerate the committed golden fixtures from their synthetic inputs."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import tempfile
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any
from urllib.parse import urljoin

os.environ.update(
    IPTV_DHCP_USER="fixture-user",
    IPTV_DHCP_PASSWORD="fixture-dhcp-password",
    IPTV_USER_ID="fixture-user",
    IPTV_PASSWORD="fixture-epg-password",
)
os.environ["TZ"] = "<+08>-8"
time.tzset()

from Crypto.Cipher import DES
from Crypto.Util.Padding import pad
from pydantic import ValidationError

from iptv_client import ctc_dhcp, web
from iptv_client.auth import make_authenticator
from iptv_client.errors import IptvError
from iptv_client.models import (
    BootChannel,
    Channel,
    ChannelListResponse,
    HlsAddress,
    Programme,
    ProgrammeResponse,
    TvodAddress,
)
from iptv_client.parsers import parse_boot_channels
from iptv_client.service import IptvService, _day_ranges
from iptv_client.xmltv import render_m3u, render_xmltv

ROOT = Path(__file__).resolve().parents[2]
FIXTURES = ROOT / "rust/tests/fixtures"


def json_text(value: Any) -> bytes:
    return (
        json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
    ).encode()


def error(kind: str, operation: str, detail: str | None = None) -> dict[str, Any]:
    value: dict[str, Any] = {"kind": kind, "operation": operation}
    if detail is not None:
        value["detail"] = detail
    return {"error": value}


def option60(case: dict[str, Any]) -> dict[str, Any]:
    data = case["input"]
    if data.get("direction") == "decode":
        kinds = {
            "prefix": "option60-malformed-prefix",
            "length": "option60-malformed-length",
            "method": "option60-malformed-method",
        }
        return error(kinds[data["violates"]], "dhcp")
    password = data.get("password", "fixture-dhcp-password")
    try:
        wire = ctc_dhcp.make_option60(
            password=password,
            login=data["login"],
            r=bytes.fromhex(data["r_hex"]),
            ts=bytes.fromhex(data["ts_hex"]),
        )
    except ValueError as exc:
        return error("option60-oversized-payload", "dhcp", str(exc))
    return {
        "wire_hex": wire.hex(),
        "declared_length": wire[3],
        "hash_key_hex": hashlib.md5(
            bytes.fromhex(data["r_hex"])
            + password.encode()
            + bytes.fromhex(data["ts_hex"])
        ).hexdigest(),
        "ciphertext_hex": wire[37:].hex(),
    }


def guide(case: dict[str, Any]) -> dict[str, Any]:
    data = case["input"]
    kind = data["kind"]
    if kind == "render-cache":
        return {"rule": "cache-keyed-by-base-url"}
    if kind == "day-ranges":
        now = datetime.fromisoformat(data["now"])
        return {
            "ranges": [
                list(pair)
                for pair in _day_ranges(now, data["past_days"], data["future_days"])
            ]
        }
    programmes = [Programme.model_validate(item) for item in data.get("programmes", [])]
    if kind == "persist":
        return {
            "file_text": json.dumps(
                [p.model_dump(by_alias=True) for p in programmes],
                ensure_ascii=False,
                indent=2,
            )
            + "\n"
        }
    with tempfile.NamedTemporaryFile("w", encoding="utf-8") as handle:
        handle.write(data["file_text"])
        handle.flush()
        try:
            loaded = IptvService._load_guide_cache(Path(handle.name))
        except IptvError as exc:
            kind_by_detail = {
                "guide cache must contain a JSON list": "guide-cache-not-a-list"
            }
            kind_name = kind_by_detail.get(
                exc.detail,
                "guide-cache-invalid-programme"
                if case["id"].endswith("invalid-programme")
                else "guide-cache-invalid-json",
            )
            detail = (
                exc.detail
                if exc.detail == "guide cache must contain a JSON list"
                else None
            )
            return error(kind_name, "guide-cache", detail)
    return {"programmes": [p.model_dump() for p in loaded or ()]}


class FixtureSecrets:
    def __init__(self) -> None:
        self.counter = 0

    def token_urlsafe(self, nbytes: int) -> str:
        del nbytes
        self.counter += 1
        return f"fixturetok{self.counter:014d}"


class FixtureClock:
    def __init__(self) -> None:
        self.now_ms = 0

    def monotonic(self) -> float:
        return self.now_ms / 1000


def hls(case: dict[str, Any]) -> dict[str, Any]:
    secrets = FixtureSecrets()
    clock = FixtureClock()
    web.secrets.__dict__["token_urlsafe"] = lambda nbytes=None: secrets.token_urlsafe(
        nbytes or 18
    )
    web.time.__dict__["monotonic"] = lambda: clock.monotonic()
    proxy = web.HlsProxy(IptvService.__new__(IptvService))
    data = case["input"]
    if data["kind"] == "rewrite":
        output = proxy._rewrite_playlist(data["body"].encode(), data["base_url"])
        resources = [
            {"token": token, "url": resource.url, "suffix": token[24:]}
            for token, resource in proxy._resources.items()
        ]
        return {"output": output.decode(), "resources": resources}
    clock.now_ms = data["register_at_ms"]
    token = proxy._register(data["url"], data["suffix"])
    result: dict[str, object] = {"token": token, "url": data["url"]}
    for now_ms in data["resolve_at_ms"]:
        clock.now_ms = now_ms
        try:
            proxy._resolve(token)
        except IptvError as exc:
            return error("hls-resource-expired", "hls-proxy", exc.detail)
    return result


def render(case: dict[str, Any]) -> dict[str, Any]:
    data = case["input"]
    channels = [BootChannel.model_validate(item) for item in data["channels"]]
    programmes = [Programme.model_validate(item) for item in data["programmes"]]
    if data["renderer"] == "m3u":
        return {
            "text": render_m3u(
                channels, data["base_url"], catchup_days=data["catchup_days"]
            )
        }
    offset = timezone(timedelta(minutes=data["timezone_offset_minutes"]))
    return {
        "text": render_xmltv(
            channels,
            programmes,
            timezone=offset,
            base_url=data["base_url"],
        ).decode()
    }


MODELS = {
    "Channel": Channel,
    "Programme": Programme,
    "TvodAddress": TvodAddress,
    "HlsAddress": HlsAddress,
    "BootChannel": BootChannel,
    "ProgrammeResponse": ProgrammeResponse,
    "ChannelListResponse": ChannelListResponse,
}


def model(case: dict[str, Any]) -> dict[str, Any]:
    data = case["input"]
    validator = MODELS[data["model"]]
    try:
        parsed = validator.model_validate(data["payload"])
    except ValidationError:
        return error("models-validation", "epg-http")
    return {"fields": parsed.model_dump()}


def expected(family: str, case: dict[str, Any]) -> dict[str, Any]:
    data = case["input"]
    if family == "padding":
        plain = bytes.fromhex(data["plaintext_hex"])
        if data["kind"] == "pkcs7":
            return {"padded_hex": pad(plain, DES.block_size).hex()}
        return {"padded_hex": (plain + b"\0" * (8 - len(plain) % 8)).hex()}
    if family == "option60":
        return option60(case)
    if family == "epg-auth":
        return {"authenticator_hex": make_authenticator(**data)}
    if family == "catchup":
        from iptv_client.web import _catchup_time_ms

        try:
            return {"start_time_ms": _catchup_time_ms(data["utc"])}
        except IptvError as exc:
            kind = (
                "catchup-missing-time"
                if exc.detail == "catch-up start time is required"
                else "catchup-invalid-time"
            )
            return error(kind, "epg-replay", exc.detail)
    if family == "guide-cache":
        return guide(case)
    if family == "hls-rewrite":
        return hls(case)
    if family == "render":
        return render(case)
    if family == "models":
        return model(case)
    if family == "parsers":
        try:
            return {
                "channels": [
                    channel.model_dump()
                    for channel in parse_boot_channels(data["body"])
                ]
            }
        except IptvError as exc:
            return error("parsers-no-boot-channels", "epg-channels", exc.detail)
    if family == "status":
        if data["kind"] == "http-error":
            status = 502 if data["operation"] == "hls-proxy" else 503
            body = {"error": str(IptvError(data["operation"], data["detail"]))}
            return {"status": status, "body": body}
        payload = data["payload"]
        if payload["status"] == "1":
            return {"result": "ok"}
        detail = payload["errMsg"] or f"status={payload['status']}"
        return error("provider-status-failure", data["operation"], detail)
    if family == "urljoin":
        return {"url": urljoin(data["base"], data["reference"])}
    raise ValueError(f"unknown fixture family: {family}")


def build_corpus() -> dict[str, bytes]:
    source_manifest = json.loads((FIXTURES / "manifest.json").read_text())
    corpus: dict[str, bytes] = {}
    files = []
    for metadata in source_manifest["files"]:
        filename = metadata["path"]
        document = json.loads((FIXTURES / filename).read_text())
        cases = [
            {**case, "expected": expected(document["family"], case)}
            for case in document["cases"]
        ]
        payload = json_text({"family": document["family"], "cases": cases})
        corpus[filename] = payload
        files.append(
            {
                "path": filename,
                "family": document["family"],
                "sha256": hashlib.sha256(payload).hexdigest(),
                "case_count": len(cases),
                "covers": sorted({cover for case in cases for cover in case["covers"]}),
            }
        )
    corpus["manifest.json"] = json_text({**source_manifest, "files": files})
    return corpus


def check(output_dir: Path, corpus: dict[str, bytes]) -> list[str]:
    on_disk = (
        {
            path.name: path.read_bytes()
            for path in output_dir.glob("*")
            if path.is_file()
        }
        if output_dir.is_dir()
        else {}
    )
    return (
        [f"missing: {name}" for name in corpus if name not in on_disk]
        + [
            f"changed: {name}"
            for name in corpus
            if name in on_disk and on_disk[name] != corpus[name]
        ]
        + [f"extra: {name}" for name in on_disk if name not in corpus]
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--check", action="store_true")
    arguments = parser.parse_args()
    corpus = build_corpus()
    drift = check(arguments.output_dir, corpus)
    if arguments.check:
        if drift:
            print("DRIFT DETECTED:", *drift, sep="\n")
            return 1
        print(f"OK ({len(corpus)} files match)")
        return 0
    arguments.output_dir.mkdir(parents=True, exist_ok=True)
    for name, payload in corpus.items():
        (arguments.output_dir / name).write_bytes(payload)
    print(f"WROTE ({len(corpus)} files)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
