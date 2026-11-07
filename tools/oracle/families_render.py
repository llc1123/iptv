"""M3U/XMLTV rendering and URL-join golden cases.

Rendering cases execute ``iptv_client.xmltv`` with fixed channel/programme
sets, a fixed base URL, and a fixed +08:00 guide timezone.
"""

from __future__ import annotations

from datetime import timedelta, timezone, tzinfo
from typing import Final
from urllib.parse import urljoin

from support import (
    RENDER_OFFSET_MINUTES,
    Case,
    make_channel,
    make_programme,
)

from iptv_client.models import BootChannel, Programme
from iptv_client.xmltv import render_m3u, render_xmltv

GUIDE_TZ: Final[tzinfo] = timezone(timedelta(minutes=RENDER_OFFSET_MINUTES))

_BASE: Final = "http://192.0.2.1:8080"
_UPSTREAM: Final = "http://upstream.example.com:8080/ts"


def _plain_channel(channel_id: str, name: str, mix_no: str) -> dict[str, object]:
    return {
        "ChannelID": channel_id,
        "ChannelName": name,
        "UserChannelID": mix_no,
        "TimeShiftURL": f"{_UPSTREAM}/{channel_id}",
    }


def _render_input(
    renderer: str,
    channels: list[BootChannel],
    programmes: list[Programme],
    base_url: str,
    catchup_days: int,
) -> dict[str, object]:
    return {
        "renderer": renderer,
        "channels": [channel.model_dump(by_alias=True) for channel in channels],
        "programmes": [programme.model_dump(by_alias=True) for programme in programmes],
        "base_url": base_url,
        "catchup_days": catchup_days,
        "timezone_offset_minutes": RENDER_OFFSET_MINUTES,
    }


def render_cases() -> list[Case]:
    """M3U and XMLTV renderer cases with escaping, quoting, and dedupe quirks."""
    plain = [
        make_channel(_plain_channel("c1", "CCTV-1", "1")),
        make_channel(_plain_channel("c2", "CCTV-2", "2")),
    ]
    catchup_channel = make_channel(
        {
            **_plain_channel("c1", "CCTV-1", "1"),
            "TimeShift": "1",
            "TimeShiftLength": "7200",
            "group_title": "Sports",
            "ChannelLogURL": "http://upstream.example.com/logo/c1.png",
        }
    )
    quoted = make_channel(
        {
            **_plain_channel("ch 1&2", 'Rock & "Roll" <Live>', "3"),
            "ChannelLogURL": "http://upstream.example.com/logo/ch%201.png",
        }
    )
    programmes = [
        make_programme(
            {
                "name": "Morning & <News>",
                "startTime": 1699971200000,
                "endTime": 1699974800000,
                "ID": "prog/1",
                "channelID": "c1",
            }
        ),
        make_programme(
            {
                "name": "Evening News",
                "startTime": 1699974800000,
                "endTime": 1699978400000,
                "ID": "prog-2",
                "channelID": "c1",
            }
        ),
    ]
    quoted_programmes = [
        make_programme(
            {
                "name": "Morning & <News>",
                "startTime": 1699971200000,
                "endTime": 1699974800000,
                "ID": "prog/1",
                "channelID": "ch 1&2",
            }
        )
    ]
    duplicate = make_programme(
        {
            "name": "Evening News",
            "startTime": 1699974800000,
            "endTime": 1699978400000,
            "ID": "prog-2",
            "channelID": "c1",
        }
    )
    unknown_channel = make_programme(
        {
            "name": "Ghost",
            "startTime": 1699967600000,
            "endTime": 1699971200000,
            "ID": "prog-ghost",
            "channelID": "missing",
        }
    )
    return [
        Case(
            id="render-m3u-basic",
            covers=("m3u",),
            input=_render_input("m3u", plain, [], _BASE, 0),
            expected={"text": render_m3u(plain, _BASE)},
        ),
        Case(
            id="render-m3u-catchup-append",
            covers=("m3u",),
            input=_render_input("m3u", [catchup_channel], [], _BASE, 7),
            expected={"text": render_m3u([catchup_channel], _BASE, catchup_days=7)},
        ),
        Case(
            id="render-m3u-catchup-disabled-with-zero-days",
            covers=("m3u",),
            input=_render_input("m3u", [catchup_channel], [], _BASE, 0),
            expected={"text": render_m3u([catchup_channel], _BASE, catchup_days=0)},
        ),
        Case(
            id="render-m3u-name-not-escaped",
            covers=("m3u", "xml-escaping"),
            input=_render_input("m3u", [quoted], [], _BASE, 0),
            expected={"text": render_m3u([quoted], _BASE)},
        ),
        Case(
            id="render-m3u-trailing-slash-base",
            covers=("m3u",),
            input=_render_input("m3u", plain, [], f"{_BASE}/", 0),
            expected={"text": render_m3u(plain, f"{_BASE}/")},
        ),
        Case(
            id="render-xmltv-escaping-and-replay-url",
            covers=("xmltv", "xml-escaping"),
            input=_render_input("xmltv", [quoted], quoted_programmes, _BASE, 0),
            expected={
                "text": render_xmltv(
                    [quoted], quoted_programmes, timezone=GUIDE_TZ, base_url=_BASE
                ).decode("utf-8")
            },
        ),
        Case(
            id="render-xmltv-dedupe-and-unknown-channel",
            covers=("xmltv",),
            input=_render_input(
                "xmltv", plain, [duplicate, unknown_channel, programmes[1]], _BASE, 0
            ),
            expected={
                "text": render_xmltv(
                    plain,
                    [duplicate, unknown_channel, programmes[1]],
                    timezone=GUIDE_TZ,
                    base_url=_BASE,
                ).decode("utf-8")
            },
        ),
        Case(
            id="render-xmltv-timezone-offset",
            covers=("xmltv",),
            input=_render_input("xmltv", plain, programmes, _BASE, 0),
            expected={
                "text": render_xmltv(
                    plain, programmes, timezone=GUIDE_TZ, base_url=_BASE
                ).decode("utf-8")
            },
        ),
    ]


def urljoin_cases() -> list[Case]:
    """URL resolution cases used by playlist rewriting."""
    base = "http://upstream.example.com:8080/live/chan.m3u8"
    references = (
        ("relative-file", "seg1.ts"),
        ("relative-parent", "../x.ts"),
        ("absolute-path", "/abs.ts"),
        ("protocol-relative", "//cdn.example.com/s.ts"),
        ("query-on-reference", "seg.ts?token=1"),
        ("query-only", "?start=1"),
        ("absolute-url", "http://other.example.com/z.ts"),
        ("fragment-only", "#frag"),
        ("empty-reference", ""),
    )
    return [
        Case(
            id=f"urljoin-{label}",
            covers=_urljoin_covers(reference),
            input={"base": base, "reference": reference},
            expected={"url": urljoin(base, reference)},
        )
        for label, reference in references
    ]


def _urljoin_covers(reference: str) -> tuple[str, ...]:
    if reference.startswith("//"):
        return ("url-join", "protocol-relative-urls")
    if reference.startswith("?"):
        return ("url-join", "query-urls")
    if "://" in reference:
        return ("url-join",)
    return ("url-join", "relative-urls")
