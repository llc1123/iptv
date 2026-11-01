from __future__ import annotations

import re
import secrets
import time
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from datetime import datetime
from urllib.parse import urljoin, urlsplit

import aiohttp.web
import httpx2

from .errors import IptvError
from .service import IptvService

_URI_ATTRIBUTE = re.compile(r'URI="([^"]+)"')
_HLS_CONTENT_TYPES = ("mpegurl", "x-mpegurl", "vnd.apple.mpegurl")
_RESOURCE_TTL_SECONDS = 300.0
_NO_CACHE_HEADERS = {
    "Cache-Control": "no-store, no-cache, max-age=0",
    "Pragma": "no-cache",
    "Expires": "0",
}


@dataclass(frozen=True, slots=True)
class _UpstreamResource:
    url: str
    expires_at: float


class HlsProxy:
    def __init__(self, service: IptvService) -> None:
        self._service = service
        self._resources: dict[str, _UpstreamResource] = {}

    def _register(self, url: str, suffix: str = "") -> str:
        now = time.monotonic()
        self._resources = {
            token: resource
            for token, resource in self._resources.items()
            if resource.expires_at > now
        }
        token = f"{secrets.token_urlsafe(18)}{suffix}"
        self._resources[token] = _UpstreamResource(
            url=url, expires_at=now + _RESOURCE_TTL_SECONDS
        )
        return token

    def _resolve(self, token: str) -> str:
        resource = self._resources.get(token)
        now = time.monotonic()
        if resource is None or resource.expires_at <= now:
            raise IptvError("hls-proxy", "playlist resource expired")
        self._resources[token] = _UpstreamResource(
            url=resource.url,
            expires_at=now + _RESOURCE_TTL_SECONDS,
        )
        return resource.url

    async def channel_playlist(
        self, request: aiohttp.web.Request, channel_id: str
    ) -> aiohttp.web.StreamResponse:
        upstream_url = await self._service.upstream_hls_url(channel_id)
        return await self._forward(request, upstream_url)

    async def replay_playlist(
        self,
        request: aiohttp.web.Request,
        channel_id: str,
        programme_id: str,
    ) -> aiohttp.web.StreamResponse:
        upstream_url = await self._service.replay_hls_url(channel_id, programme_id)
        return await self._forward(request, upstream_url)

    async def catchup_playlist(
        self, request: aiohttp.web.Request, channel_id: str, start_time_ms: int
    ) -> aiohttp.web.StreamResponse:
        upstream_url = await self._service.replay_hls_url_for_range(
            channel_id, start_time_ms
        )
        return await self._forward(request, upstream_url)

    async def resource(
        self, request: aiohttp.web.Request, token: str
    ) -> aiohttp.web.StreamResponse:
        return await self._forward(request, self._resolve(token))

    async def _forward(
        self, request: aiohttp.web.Request, upstream_url: str
    ) -> aiohttp.web.StreamResponse:
        headers = {
            name: request.headers[name]
            for name in ("Accept", "Range", "User-Agent")
            if name in request.headers
        }
        client = self._service.http_client
        try:
            async with client.stream("GET", upstream_url, headers=headers) as upstream:
                upstream.raise_for_status()
                content_type = upstream.headers.get("content-type", "")
                is_playlist = upstream_url.lower().split("?", 1)[0].endswith(".m3u8")
                if is_playlist or any(
                    value in content_type.lower() for value in _HLS_CONTENT_TYPES
                ):
                    body = await upstream.aread()
                    return aiohttp.web.Response(
                        body=self._rewrite_playlist(body, str(upstream.url)),
                        content_type="application/vnd.apple.mpegurl",
                    )

                response_content_type = content_type
                if content_type.split(";", 1)[0].strip().lower() == "video/mpeg":
                    response_content_type = "video/mp2t"
                response = aiohttp.web.StreamResponse(
                    status=upstream.status_code,
                    headers={
                        name: (
                            response_content_type
                            if name.lower() == "content-type"
                            else value
                        )
                        for name, value in upstream.headers.items()
                        if name.lower()
                        in {
                            "content-type",
                            "content-length",
                            "content-range",
                            "accept-ranges",
                        }
                    },
                )
                await response.prepare(request)
                async for chunk in upstream.aiter_bytes():
                    await response.write(chunk)
                await response.write_eof()
                return response
        except httpx2.HTTPError as error:
            raise IptvError("hls-proxy", str(error)) from error

    def _rewrite_playlist(self, body: bytes, base_url: str) -> bytes:
        text = body.decode("utf-8", errors="replace")
        rewritten: list[str] = []
        for line in text.splitlines():
            if line.startswith("#"):
                rewritten.append(
                    _URI_ATTRIBUTE.sub(
                        lambda match: (
                            f'URI="/_hls/{self._register_uri(base_url, match.group(1))}"'
                        ),
                        line,
                    )
                )
                continue
            if line.strip():
                absolute_url = urljoin(base_url, line.strip())
                token = self._register(
                    absolute_url, self._resource_suffix(absolute_url, ".ts")
                )
                rewritten.append(f"/_hls/{token}")
            else:
                rewritten.append(line)
        return ("\n".join(rewritten) + "\n").encode("utf-8")

    def _register_uri(self, base_url: str, uri: str) -> str:
        absolute_url = urljoin(base_url, uri)
        return self._register(absolute_url, self._resource_suffix(absolute_url))

    @staticmethod
    def _resource_suffix(url: str, default: str = "") -> str:
        suffix = urlsplit(url).path.rsplit("/", 1)[-1]
        if "." not in suffix:
            return default
        extension = "." + suffix.rsplit(".", 1)[-1].lower()
        return extension if len(extension) <= 6 else default


def _service(request: aiohttp.web.Request) -> IptvService:
    return request.app["iptv_service"]


Handler = Callable[[aiohttp.web.Request], Awaitable[aiohttp.web.StreamResponse]]


async def _handle_errors(
    handler: Handler, request: aiohttp.web.Request
) -> aiohttp.web.StreamResponse:
    try:
        return await handler(request)
    except IptvError as error:
        return aiohttp.web.json_response(
            {"error": str(error)}, status=503 if error.operation != "hls-proxy" else 502
        )


async def health(request: aiohttp.web.Request) -> aiohttp.web.Response:
    service = _service(request)
    return aiohttp.web.json_response(
        {"status": "ok", "channels": len(service.channels)}
    )


async def playlist(request: aiohttp.web.Request) -> aiohttp.web.Response:
    service = _service(request)
    base_url = f"{request.scheme}://{request.host}"
    return aiohttp.web.Response(
        text=await service.playlist(base_url),
        content_type="audio/x-mpegurl",
        headers=_NO_CACHE_HEADERS,
    )


async def xmltv(request: aiohttp.web.Request) -> aiohttp.web.Response:
    return aiohttp.web.Response(
        body=await _service(request).guide(f"{request.scheme}://{request.host}"),
        content_type="application/xml",
        headers=_NO_CACHE_HEADERS,
    )


async def logo(request: aiohttp.web.Request) -> aiohttp.web.FileResponse:
    path = _service(request).logo_path(request.match_info["channel_id"])
    if path is None:
        raise IptvError("logo", "unknown or unavailable channel logo")
    return aiohttp.web.FileResponse(
        path,
        headers={"Cache-Control": "public, max-age=86400"},
    )


async def sync_metadata(request: aiohttp.web.Request) -> aiohttp.web.Response:
    return aiohttp.web.json_response(await _service(request).sync_metadata())


async def stream(request: aiohttp.web.Request) -> aiohttp.web.StreamResponse:
    proxy: HlsProxy = request.app["hls_proxy"]
    utc = request.query.get("utc")
    if utc:
        return await proxy.catchup_playlist(
            request,
            request.match_info["channel_id"],
            _catchup_time_ms(utc),
        )
    return await proxy.channel_playlist(request, request.match_info["channel_id"])


async def replay(request: aiohttp.web.Request) -> aiohttp.web.StreamResponse:
    proxy: HlsProxy = request.app["hls_proxy"]
    return await proxy.replay_playlist(
        request,
        request.match_info["channel_id"],
        request.match_info["programme_id"],
    )


def _catchup_time_ms(value: str | None) -> int:
    if not value:
        raise IptvError("epg-replay", "catch-up start time is required")
    if value.isdigit():
        timestamp = int(value)
        return timestamp if timestamp > 10_000_000_000 else timestamp * 1000
    try:
        parsed = datetime.fromisoformat(value)
    except ValueError as error:
        raise IptvError("epg-replay", "invalid catch-up start time") from error
    return int(parsed.timestamp() * 1000)


async def catchup(request: aiohttp.web.Request) -> aiohttp.web.StreamResponse:
    proxy: HlsProxy = request.app["hls_proxy"]
    return await proxy.catchup_playlist(
        request,
        request.match_info["channel_id"],
        _catchup_time_ms(request.query.get("utc") or request.query.get("start")),
    )


async def resource(request: aiohttp.web.Request) -> aiohttp.web.StreamResponse:
    proxy: HlsProxy = request.app["hls_proxy"]
    return await proxy.resource(request, request.match_info["token"])


async def _lifecycle(app: aiohttp.web.Application):
    service: IptvService = app["iptv_service"]
    await service.start()
    try:
        yield
    finally:
        await service.close()


def create_app(service: IptvService) -> aiohttp.web.Application:
    app = aiohttp.web.Application()
    app["iptv_service"] = service
    app["hls_proxy"] = HlsProxy(service)
    app.cleanup_ctx.append(_lifecycle)
    app.router.add_get("/healthz", health)
    app.router.add_get(
        "/channels.m3u", lambda request: _handle_errors(playlist, request)
    )
    app.router.add_get("/xmltv.xml", lambda request: _handle_errors(xmltv, request))
    app.router.add_get(
        "/logos/{channel_id}", lambda request: _handle_errors(logo, request)
    )
    app.router.add_post(
        "/metadata/sync", lambda request: _handle_errors(sync_metadata, request)
    )
    app.router.add_get(
        "/stream/{channel_id}.m3u8", lambda request: _handle_errors(stream, request)
    )
    app.router.add_get(
        "/replay/{channel_id}/{programme_id}.m3u8",
        lambda request: _handle_errors(replay, request),
    )
    app.router.add_get(
        "/catchup/{channel_id}.m3u8",
        lambda request: _handle_errors(catchup, request),
    )
    app.router.add_get(
        "/_hls/{token}", lambda request: _handle_errors(resource, request)
    )
    return app
