"""HLS playlist rewriting and resource-token golden cases.

The cases drive ``iptv_client.web.HlsProxy`` pure helpers with an injected
URL-safe token generator and an injected monotonic clock so the rewritten
bytes and registered resource map are reproducible.
"""

from __future__ import annotations

from typing import Final

from support import Case, FakeClock, FakeSecrets, error_expected

from iptv_client import web
from iptv_client.errors import IptvError
from iptv_client.web import HlsProxy

_TOKEN_BASE: Final = "http://upstream.example.com:8080/live/chan.m3u8"
_FIRST_TOKEN: Final = "fixturetok00000000000001"


def _proxy() -> tuple[HlsProxy, FakeSecrets, FakeClock]:
    secrets = FakeSecrets()
    clock = FakeClock()
    web.secrets = secrets
    web.time = clock
    return HlsProxy(object()), secrets, clock


def _rewrite_case(
    case_id: str,
    body: str,
    *,
    base_url: str = _TOKEN_BASE,
    covers: tuple[str, ...] = ("hls-rewrite",),
) -> Case:
    proxy, secrets, _clock = _proxy()
    secrets.reset()
    output = proxy._rewrite_playlist(body.encode("utf-8"), base_url)
    resources = [
        {
            "token": token,
            "url": resource.url,
            "suffix": token[24:],
        }
        for token, resource in proxy._resources.items()
    ]
    return Case(
        id=case_id,
        covers=covers,
        input={"kind": "rewrite", "base_url": base_url, "body": body},
        expected={"output": output.decode("utf-8"), "resources": resources},
    )


def _resource_case(
    case_id: str,
    *,
    url: str = "http://upstream.example.com:8080/live/seg1.ts",
    register_at_ms: int = 0,
    resolve_at_ms: tuple[int, ...] = (299_000,),
    covers: tuple[str, ...] = ("hls-rewrite",),
) -> Case:
    proxy, secrets, clock = _proxy()
    secrets.reset()
    clock.advance_to(register_at_ms)
    token = proxy._register(url, ".ts")
    expected: dict[str, object] = {"token": token, "url": url}
    for at_ms in resolve_at_ms:
        clock.advance_to(at_ms)
        try:
            proxy._resolve(token)
        except IptvError as error:
            expected = error_expected(
                "hls-resource-expired", operation="hls-proxy", detail=error.detail
            )
    return Case(
        id=case_id,
        covers=covers,
        input={
            "kind": "resource",
            "url": url,
            "suffix": ".ts",
            "register_at_ms": register_at_ms,
            "resolve_at_ms": list(resolve_at_ms),
        },
        expected=expected,
    )


def hls_rewrite_cases() -> list[Case]:
    """Playlist rewriting cases across URL forms and line-ending quirks."""
    return [
        _rewrite_case(
            "hls-rewrite-relative-segments",
            (
                "#EXTM3U\n"
                "#EXT-X-VERSION:3\n"
                "#EXT-X-TARGETDURATION:10\n"
                "#EXTINF:10.0,\n"
                "seg1.ts\n"
                "seg2.ts\n"
            ),
            covers=("hls-rewrite", "relative-urls"),
        ),
        _rewrite_case(
            "hls-rewrite-protocol-relative-segment",
            "#EXTM3U\n#EXTINF:10.0,\n//cdn.example.com/s.ts\n",
            covers=("hls-rewrite", "protocol-relative-urls"),
        ),
        _rewrite_case(
            "hls-rewrite-query-segments",
            "#EXTM3U\n#EXTINF:10.0,\nseg.ts?token=abc&x=1\n?start=1\n",
            covers=("hls-rewrite", "query-urls"),
        ),
        _rewrite_case(
            "hls-rewrite-uri-attribute",
            (
                "#EXTM3U\n"
                '#EXT-X-KEY:METHOD=AES-128,URI="key.bin"\n'
                '#EXT-X-MAP:URI="init.mp4"\n'
                "#EXTINF:10.0,\n"
                "seg1.ts\n"
            ),
        ),
        _rewrite_case(
            "hls-rewrite-lowercase-uri-ignored",
            '#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,uri="key.bin"\nseg1.ts\n',
        ),
        _rewrite_case(
            "hls-rewrite-crlf",
            "#EXTM3U\r\n#EXTINF:10.0,\r\nseg1.ts\r\n\r\n",
            covers=("hls-rewrite", "crlf-playlists"),
        ),
        _rewrite_case(
            "hls-rewrite-cr",
            "#EXTM3U\r#EXTINF:10.0,\rseg1.ts\r",
            covers=("hls-rewrite", "cr-playlists"),
        ),
        _rewrite_case(
            "hls-rewrite-bom-first-line-becomes-resource",
            "\ufeff#EXTM3U\n#EXTINF:10.0,\nseg1.ts\n",
            covers=("hls-rewrite", "bom-playlists"),
        ),
        _rewrite_case(
            "hls-rewrite-blank-and-whitespace-lines",
            "#EXTM3U\n\nseg1.ts\n   \nseg2.ts\n",
        ),
        _rewrite_case(
            "hls-rewrite-extension-defaults",
            (
                "#EXTM3U\n"
                "seg.verylongext\n"
                "noextseg\n"
                "UPPER.TS\n"
                '#EXT-X-KEY:METHOD=AES-128,URI="nokeyext"\n'
            ),
        ),
        _rewrite_case(
            "hls-rewrite-x-tvg-url-untouched",
            (
                '#EXTM3U x-tvg-url="http://epg.example.com/xmltv.xml"\n'
                "#EXTINF:10.0,\n"
                "http://other.example.com/z.ts\n"
            ),
        ),
    ]


def hls_resource_cases() -> list[Case]:
    """Resource-token lifetime cases against the injected monotonic clock."""
    return [
        _resource_case("hls-resource-resolve-before-expiry"),
        _resource_case(
            "hls-resource-resolve-extends-ttl",
            resolve_at_ms=(299_000, 598_000),
        ),
        _resource_case(
            "hls-resource-expiry-boundary",
            resolve_at_ms=(300_000,),
        ),
    ]
