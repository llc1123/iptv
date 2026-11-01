"""Guide-cache and catch-up start-time golden cases.

Guide-cache cases execute the POC guide render-cache rule, the guide day
window computation, and the cache-file load/persist helpers; catch-up cases
execute ``web._catchup_time_ms`` under the fixed +08:00 deployment timezone.
"""

from __future__ import annotations

import json
import tempfile
from datetime import datetime
from pathlib import Path

from support import (
    RENDER_OFFSET_MINUTES,
    Case,
    OracleSafetyError,
    error_expected,
    make_programme,
)

from iptv_client.errors import IptvError
from iptv_client.service import IptvService, _day_ranges
from iptv_client.web import _catchup_time_ms


def catchup_cases() -> list[Case]:
    """Catch-up start-time parsing cases (numeric and ISO forms)."""
    inputs = (
        ("numeric-seconds", "1700000000"),
        ("numeric-millis", "1700000000123"),
        ("numeric-threshold-exact", "10000000000"),
        ("numeric-threshold-above", "10000000001"),
        ("numeric-zero", "0"),
        ("iso-offset", "2023-11-14T22:13:20+08:00"),
        ("iso-naive-local", "2023-11-14T22:13:20"),
        ("iso-date-only", "2023-11-14"),
        ("invalid-empty", ""),
        ("invalid-text", "abc"),
        ("invalid-negative", "-5"),
        ("invalid-whitespace", " 123"),
        ("invalid-exponent", "1e6"),
    )
    cases = []
    for label, value in inputs:
        covers = ("catch-up",)
        if value.isdigit():
            covers = ("catch-up", "numeric-catchup")
        elif label.startswith("iso-"):
            covers = ("catch-up", "iso-catchup")
        try:
            start_ms = _catchup_time_ms(value)
        except IptvError as error:
            kind = (
                "catchup-missing-time"
                if error.detail == "catch-up start time is required"
                else "catchup-invalid-time"
            )
            expected = error_expected(kind, operation="epg-replay", detail=error.detail)
        else:
            expected = {"start_time_ms": start_ms}
        cases.append(
            Case(
                id=f"catchup-{label}",
                covers=covers,
                input={"utc": value},
                expected=expected,
            )
        )
    return cases


def _load_error_case(case_id: str, text: str) -> Case:
    with tempfile.TemporaryDirectory() as directory:
        cache_path = Path(directory) / "guide.json"
        cache_path.write_text(text, encoding="utf-8")
        try:
            IptvService._load_guide_cache(cache_path)
        except IptvError as error:
            kind = {
                "guide cache must contain a JSON list": "guide-cache-not-a-list",
            }.get(
                error.detail,
                "guide-cache-invalid-programme"
                if case_id.endswith("invalid-programme")
                else "guide-cache-invalid-json",
            )
            detail = (
                error.detail
                if error.detail == "guide cache must contain a JSON list"
                else None
            )
            return Case(
                id=case_id,
                covers=("guide-cache",),
                input={"kind": "load", "file_text": text},
                expected=error_expected(kind, operation="guide-cache", detail=detail),
            )
    raise OracleSafetyError(f"cache load unexpectedly accepted {case_id}")


def guide_cache_cases() -> list[Case]:
    """Guide render-cache keying, day windows, and cache-file format cases."""
    now = datetime.fromisoformat("2023-11-14T22:13:20+08:00")
    ranges = _day_ranges(now, 7, 2)
    programmes = [
        make_programme(
            {
                "name": "Morning & <News>",
                "startTime": 1699971200000,
                "endTime": 1699974800000,
                "ID": "prog/1",
                "channelID": "c1",
            }
        )
    ]
    file_text = (
        json.dumps(
            [programme.model_dump(by_alias=True) for programme in programmes],
            ensure_ascii=False,
            indent=2,
        )
        + "\n"
    )
    with tempfile.TemporaryDirectory() as directory:
        cache_path = Path(directory) / "guide.json"
        cache_path.write_text(file_text, encoding="utf-8")
        loaded = IptvService._load_guide_cache(cache_path)
    return [
        Case(
            id="guide-cache-render-keyed-by-base-url",
            covers=("guide-cache",),
            input={
                "kind": "render-cache",
                "steps": [
                    {"base_url": "http://192.0.2.1:8080", "expect": "render"},
                    {"base_url": "http://192.0.2.1:8080", "expect": "cached"},
                    {"base_url": "http://192.0.2.1:8080/other", "expect": "render"},
                    {"event": "refresh"},
                    {"base_url": "http://192.0.2.1:8080/other", "expect": "render"},
                ],
            },
            expected={"rule": "cache-keyed-by-base-url"},
        ),
        Case(
            id="guide-cache-day-ranges",
            covers=("guide-cache",),
            input={
                "kind": "day-ranges",
                "now": "2023-11-14T22:13:20+08:00",
                "past_days": 7,
                "future_days": 2,
                "timezone_offset_minutes": RENDER_OFFSET_MINUTES,
            },
            expected={"ranges": [list(pair) for pair in ranges]},
        ),
        Case(
            id="guide-cache-persist-format",
            covers=("guide-cache",),
            input={
                "kind": "persist",
                "programmes": [p.model_dump(by_alias=True) for p in programmes],
            },
            expected={"file_text": file_text},
        ),
        Case(
            id="guide-cache-load-valid",
            covers=("guide-cache",),
            input={"kind": "load", "file_text": file_text},
            expected={"programmes": [p.model_dump() for p in loaded or ()]},
        ),
        _load_error_case("guide-cache-load-not-a-list", "{}"),
        _load_error_case("guide-cache-load-invalid-json", "{not json"),
        _load_error_case("guide-cache-load-invalid-programme", '[{"name": "x"}]'),
    ]
