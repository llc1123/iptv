#!/usr/bin/env python3
"""Generate deterministic golden fixtures from the Python POC oracle.

Usage (from the repository root):

    uv run python tools/oracle/generate.py --output-dir rust/tests/fixtures
    uv run python tools/oracle/generate.py --check --output-dir rust/tests/fixtures

The generator executes the pure POC behavior in ``iptv_client`` with injected
randomness, monotonic time, URL-safe tokens, guide time, and base URLs. It
never reads runtime credentials: ``main`` installs an audit hook (refusing
network access and any read of ``var/`` or the gitignored E2E environment
file) before importing any family or ``iptv_client`` module, so POC import
time behavior runs under the guard. ``--check`` verifies
that the output directory matches the generated corpus byte for byte and
exits nonzero when any fixture drifted.
"""

from __future__ import annotations

import argparse
import sys
from collections.abc import Callable
from pathlib import Path

from support import (
    CORPUS_DIGEST,
    FORBIDDEN_MARKERS,
    REQUIRED_COVERAGE,
    SCHEMA,
    SCHEMA_VERSION,
    Case,
    OracleSafetyError,
    corpus_digest,
    disk_corpus_digest,
    force_timezone,
    install_safety_guards,
    json_text,
    sha256_hex,
)

FamilyBuilder = Callable[[], list[Case]]
FamilySpec = tuple[str, str, FamilyBuilder]


def family_builders() -> tuple[FamilySpec, ...]:
    """Return the fixture family builders.

    The family modules import ``iptv_client`` (whose ``dhcp`` module reads
    environment configuration at import time), so they are imported here,
    after ``main`` has installed the safety guard, never at module load.
    """
    from families_crypto import epg_auth_cases, option60_cases, padding_cases
    from families_guide import catchup_cases, guide_cache_cases
    from families_hls import hls_resource_cases, hls_rewrite_cases
    from families_provider import model_cases, parser_cases
    from families_render import render_cases, urljoin_cases
    from families_status import status_shape_cases

    return (
        ("padding.json", "padding", padding_cases),
        ("option60.json", "option60", option60_cases),
        ("epg_auth.json", "epg-auth", epg_auth_cases),
        ("parsers.json", "parsers", parser_cases),
        ("models.json", "models", model_cases),
        ("status_shapes.json", "status", status_shape_cases),
        ("m3u_xmltv.json", "render", render_cases),
        ("guide_cache.json", "guide-cache", guide_cache_cases),
        ("urljoin.json", "urljoin", urljoin_cases),
        ("catchup.json", "catchup", catchup_cases),
        (
            "hls_rewrite.json",
            "hls-rewrite",
            lambda: hls_rewrite_cases() + hls_resource_cases(),
        ),
    )


def build_corpus() -> dict[str, bytes]:
    """Build the full fixture corpus as ``path -> file bytes`` mappings."""
    corpus: dict[str, bytes] = {}
    manifest_files = []
    covered: set[str] = set()
    for filename, family_name, builder in family_builders():
        cases = builder()
        ids = [case.id for case in cases]
        if len(set(ids)) != len(ids):
            raise OracleSafetyError(f"duplicate case id in {filename}")
        for case in cases:
            covered.update(case.covers)
        document = {
            "family": family_name,
            "cases": [case.to_json() for case in cases],
        }
        payload = json_text(document).encode("utf-8")
        corpus[filename] = payload
        manifest_files.append(
            {
                "path": filename,
                "family": family_name,
                "sha256": sha256_hex(payload),
                "case_count": len(cases),
                "covers": sorted({cover for case in cases for cover in case.covers}),
            }
        )
    missing = sorted(set(REQUIRED_COVERAGE) - covered)
    if missing:
        raise OracleSafetyError(f"required coverage is missing cases: {missing}")
    manifest = {
        "schema": SCHEMA,
        "version": SCHEMA_VERSION,
        "about": (
            "Deterministic golden fixtures generated from the Python POC oracle. "
            "All inputs are synthetic constants declared below; expected values "
            "are produced by executing iptv_client code, never by hand."
        ),
        "timezone": {
            "local_posix": "<+08>-8",
            "render_offset_minutes": 480,
        },
        "required_coverage": list(REQUIRED_COVERAGE),
        "forbidden_markers_sha256": sha256_hex(
            "\n".join(sorted(FORBIDDEN_MARKERS)).encode("utf-8")
        ),
        "inputs": {
            "dhcp_user": "fixture-user",
            "dhcp_password": "fixture-dhcp-password",
            "epg_user": "fixture-user",
            "epg_password": "fixture-epg-password",
            "encrypt_token": "FIXTURE-ENCRYPT-TOKEN",
            "stb_id": "0102030405060708",
            "ip_address": "192.0.2.10",
            "mac": "02:00:00:00:00:01",
            "custom_str": "$CTC",
            "random_value": 12345678,
            "r_hex": "7856341200000000",
            "ts_hex": "cb047b4514000000",
            "base_url": "http://192.0.2.1:8080",
            "upstream_base": "http://upstream.example.com:8080/live/chan.m3u8",
            "token_prefix": "fixturetok",
        },
        "files": manifest_files,
    }
    corpus["manifest.json"] = json_text(manifest).encode("utf-8")
    return corpus


def scan_forbidden(output_dir: Path, corpus: dict[str, bytes]) -> None:
    """Refuse fixtures carrying runtime-secret or host-path markers.

    The marker list itself travels in the manifest only as a SHA-256 digest so
    that no fixture file, including the manifest, contains a marker literal.
    """
    for name, payload in corpus.items():
        text = payload.decode("utf-8")
        for marker in FORBIDDEN_MARKERS:
            if marker in text:
                raise OracleSafetyError(
                    f"fixture {name} contains forbidden marker {marker!r}"
                )
    if output_dir.is_dir():
        for path in sorted(output_dir.iterdir()):
            if path.is_file() and path.name.endswith(".json"):
                text = path.read_text(encoding="utf-8")
                for marker in FORBIDDEN_MARKERS:
                    if marker in text:
                        raise OracleSafetyError(
                            f"on-disk fixture {path.name} contains forbidden marker {marker!r}"
                        )


def write_outputs(output_dir: Path, corpus: dict[str, bytes]) -> None:
    """Write the corpus to ``output_dir`` with atomic per-file replacement.

    Each file is staged as ``.<name>.tmp`` and swapped in with an atomic
    replace. The ``finally`` unlink keeps any unwinding interruption
    (including ``KeyboardInterrupt`` from SIGINT) from leaving a staged file
    behind; it is idempotent because a successful replace already consumed
    the staging path.
    """
    output_dir.mkdir(parents=True, exist_ok=True)
    for name in sorted(corpus):
        target = output_dir / name
        temporary = output_dir / f".{name}.tmp"
        try:
            temporary.write_bytes(corpus[name])
            temporary.replace(target)
        finally:
            temporary.unlink(missing_ok=True)


def check_outputs(output_dir: Path, corpus: dict[str, bytes]) -> list[str]:
    """Return drift descriptions when ``output_dir`` differs from the corpus."""
    drift = []
    on_disk = (
        {
            path.name: path.read_bytes()
            for path in sorted(output_dir.iterdir())
            if path.is_file()
        }
        if output_dir.is_dir()
        else {}
    )
    for name in sorted(corpus):
        if name not in on_disk:
            drift.append(f"missing: {name}")
        elif on_disk[name] != corpus[name]:
            drift.append(f"changed: {name}")
    for name in sorted(on_disk):
        if name not in corpus:
            drift.append(f"extra: {name}")
    return drift


def main() -> int:
    """CLI entry point: generate or drift-check the fixture corpus."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--check", action="store_true")
    arguments = parser.parse_args()

    repo_root = Path(__file__).resolve().parents[2]
    try:
        install_safety_guards(repo_root)
        force_timezone()
        corpus = build_corpus()
        scan_forbidden(arguments.output_dir, corpus)
        generated_digest = corpus_digest(corpus, Path("rust/tests/fixtures"))
        if generated_digest != CORPUS_DIGEST:
            raise OracleSafetyError(
                f"generated corpus digest drifted: {generated_digest}"
            )
    except OracleSafetyError as error:
        print(f"REFUSED: {error}", file=sys.stderr)
        return 2

    if arguments.check:
        drift = check_outputs(arguments.output_dir, corpus)
        if drift:
            print("DRIFT DETECTED:", file=sys.stderr)
            for entry in drift:
                print(f"  {entry}", file=sys.stderr)
            return 1
        for name in sorted(corpus):
            print(f"{sha256_hex(corpus[name])}  {name}")
        print(f"CORPUS_DIGEST={disk_corpus_digest(arguments.output_dir)}")
        print(f"OK ({len(corpus)} files match)")
        return 0

    write_outputs(arguments.output_dir, corpus)
    for name in sorted(corpus):
        print(f"{sha256_hex(corpus[name])}  {name}")
    print(f"CORPUS_DIGEST={disk_corpus_digest(arguments.output_dir)}")
    print(f"WROTE ({len(corpus)} files)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
