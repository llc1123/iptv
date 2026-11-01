from __future__ import annotations

import re
import socket
from dataclasses import dataclass
from ipaddress import IPv4Address
from types import TracebackType
from typing import Final, Self, TypedDict, Unpack
from urllib.parse import unquote, urlsplit

import httpx2

from .auth import identity_encode, make_authenticator
from .errors import IptvError
from .models import (
    BootChannel,
    ChannelListResponse,
    HlsAddress,
    Programme,
    ProgrammeResponse,
    TvodAddress,
    TvodAddressResponse,
)
from .parsers import parse_boot_channels

# The actual EDS URL is deployment-specific and must be supplied with
# IPTV_AUTH_URL. This non-routable placeholder prevents accidental requests
# when credentials or network configuration have not been set.
EPG_ORIGIN: Final = "https://example.invalid"
EDS_AUTH_URL: Final = f"{EPG_ORIGIN}/EDS/jsp/AuthenticationURL"
CHANNELS_PATH: Final = "/EPG/jsp/getchannellistHWCU.jsp"
HLS_PATH: Final = "/EPG/jsp/jkjiaoyutest/en/epg4ChannelPlayByAjax.jsp"
CHANNEL_AJAX_PATH: Final = (
    "/EPG/jsp/jkjiaoyutest/en/function/ajax/epg7getChannelByAjax.jsp"
)
LIVE_CATEGORY_ID: Final = "10000100000000090000000000001692"
AJAX_HEADERS: Final = {"VIS-AJAX": "AjaxHttpRequest", "Accept": "*/*"}
STB_GROUP_CATEGORIES: Final = (
    ("catauto5122", "4K超高清"),
    ("catauto2625", "央视"),
    ("10000100000000090000000000001691", "本地"),
    ("10000100000000090000000000001689", "卫视"),
    ("catauto2561", "新闻"),
    ("10000100000000090000000000001690", "高清"),
    ("10000100000000090000000000005665", "付费频道"),
)


@dataclass(frozen=True, slots=True)
class EpgConfig:
    user_id: str
    stb_id: str
    stb_mac: str
    password: str
    authentication_url: str = EDS_AUTH_URL
    stb_type: str = "E900V21D"
    area_id: str = "021"
    user_group_id: str = "2025"


class RequestKwargs(TypedDict, total=False):
    params: dict[str, str]
    data: dict[str, str]
    headers: dict[str, str]


@dataclass(frozen=True, slots=True)
class ProgrammeQuery:
    channel_id: str
    channel_code: str
    start_time_ms: int
    end_time_ms: int


def _make_client(source_ip: IPv4Address) -> httpx2.AsyncClient:
    limits = httpx2.Limits(
        max_connections=20, max_keepalive_connections=10, keepalive_expiry=30.0
    )
    timeout = httpx2.Timeout(connect=5.0, read=30.0, write=10.0, pool=10.0)
    transport = httpx2.AsyncHTTPTransport(
        http2=False,
        retries=3,
        limits=limits,
        local_address=str(source_ip),
        socket_options=[(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)],
    )
    return httpx2.AsyncClient(
        base_url=EPG_ORIGIN,
        transport=transport,
        timeout=timeout,
        follow_redirects=True,
        trust_env=False,
        headers={
            "User-Agent": "Mozilla/5.0 (X11; Linux x86_64; SkyworthBrowser) AppleWebKit/534.24 (KHTML, like Gecko) Safari/534.24 SkWebKit-SH-CU",
            "Accept-Language": "zh-CN,en-US;q=0.9",
            "X-Requested-With": "com.skyworth.iptv",
        },
    )


def _resolve_url(origin: str, path: str) -> str:
    return path if path.startswith(("http://", "https://")) else f"{origin}{path}"


class EpgClient:
    def __init__(self, config: EpgConfig, source_ip: IPv4Address) -> None:
        self._config = config
        self._client = _make_client(source_ip)
        self._source_ip = source_ip
        self._origin = EPG_ORIGIN
        self._user_token = ""
        self._temp_key = ""

    async def __aenter__(self) -> Self:
        await self._client.__aenter__()
        return self

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        await self._client.__aexit__(exc_type, exc, tb)

    @property
    def http_client(self) -> httpx2.AsyncClient:
        return self._client

    async def _request(
        self, method: str, path: str, **kwargs: Unpack[RequestKwargs]
    ) -> httpx2.Response:
        try:
            url = _resolve_url(self._origin, path)
            response = await self._client.request(method, url, **kwargs)
            response.raise_for_status()
        except httpx2.HTTPError as error:
            raise IptvError("epg-http", str(error)) from error
        return response

    async def authenticate(self) -> None:
        discovery = await self._request(
            "GET",
            self._config.authentication_url,
            params={"UserID": self._config.user_id, "Action": "Login", "Mode": "MENU"},
        )
        discovered = urlsplit(str(discovery.url))
        if not discovered.hostname or discovered.port is None:
            raise IptvError(
                "epg-discovery", "AuthenticationURL did not resolve to an EPG origin"
            )
        self._origin = f"{discovered.scheme}://{discovered.netloc}"
        login_response = await self._request(
            "POST",
            "/EPG/jsp/authLoginHWCU.jsp",
            data={"UserID": self._config.user_id, "VIP": ""},
        )
        encrypt_token = self._extract(login_response.text, "EncryptToken")
        if not encrypt_token:
            raise IptvError(
                "epg-auth", "authLogin response did not expose EncryptToken"
            )
        auth_value = make_authenticator(
            encrypt_token=encrypt_token,
            user_id=self._config.user_id,
            stb_id=self._config.stb_id,
            ip_address=str(self._source_ip),
            mac=self._config.stb_mac,
            password=self._config.password,
        )
        payload = {
            "UserID": self._config.user_id,
            "Lang": "0",
            "SupportHD": "1",
            "NetUserID": "",
            "Authenticator": auth_value,
            "STBType": self._config.stb_type,
            "STBVersion": "",
            "conntype": "dhcp",
            "STBID": self._config.stb_id,
            "templateName": "EPG8",
            "areaId": self._config.area_id,
            "userToken": encrypt_token,
            "userGroupId": self._config.user_group_id,
            "productPackageId": "",
            "mac": self._config.stb_mac,
            "SoftwareVersion": "",
            "VIP": "",
        }
        response = await self._request(
            "POST", "/EPG/jsp/ValidAuthenticationHWCU.jsp", data=payload
        )
        body = response.text
        self._user_token = self._extract_js_config(body, "UserToken")
        session_id = self._extract_js_config(body, "SessionID")
        share_key = self._extract_js_config(body, "shareKey")
        if not self._user_token:
            raise IptvError(
                "epg-auth", "ValidAuthentication response did not expose UserToken"
            )
        if not session_id:
            raise IptvError(
                "epg-auth", "ValidAuthentication response did not expose SessionID"
            )
        self._temp_key = identity_encode(session_id, share_key)

    @staticmethod
    def _extract(body: str, name: str) -> str:
        match = re.search(
            rf"(?:[?&]|\b){re.escape(name)}\s*=\s*[\"']?([^&\"'<>\s]+)", body
        )
        return unquote(match.group(1)) if match else ""

    @staticmethod
    def _extract_js_config(body: str, name: str) -> str:
        match = re.search(
            rf"(?:CU|CTC)SetConfig\('{re.escape(name)}','([^']*)'\)", body
        )
        return match.group(1) if match else ""

    async def channels(self) -> list[BootChannel]:
        response = await self._request(
            "POST",
            CHANNELS_PATH,
            data={
                "conntype": "dhcp",
                "UserToken": self._user_token,
                "tempKey": self._temp_key,
                "stbid": "310019",
                "SupportHD": "1",
                "UserID": self._config.user_id,
                "Lang": "1",
            },
        )
        channels = parse_boot_channels(response.text)
        codes = await self.channel_codes()
        groups = await self.channel_groups()
        return [
            channel.model_copy(
                update={
                    "code": codes.get(str(channel.channel_id), ""),
                    "group_title": groups.get(str(channel.channel_id), "全部"),
                }
            )
            for channel in channels
        ]

    async def _channel_list(self, category_id: str) -> ChannelListResponse:
        response = await self._request(
            "POST",
            CHANNEL_AJAX_PATH,
            data={
                "action": "getChannelList",
                "cateID": category_id,
                "type": "",
            },
            headers=AJAX_HEADERS,
        )
        payload = ChannelListResponse.model_validate(response.json())
        if payload.status != "1":
            raise IptvError(
                "epg-channel-codes",
                payload.error_message or f"status={payload.status}",
            )
        return payload

    async def channel_codes(self) -> dict[str, str]:
        payload = await self._channel_list(LIVE_CATEGORY_ID)
        return {
            str(channel.epg_id): str(channel.code)
            for channel in payload.data
            if channel.code
        }

    async def channel_groups(self) -> dict[str, str]:
        groups: dict[str, str] = {}
        for category_id, title in STB_GROUP_CATEGORIES:
            try:
                payload = await self._channel_list(category_id)
            except IptvError:
                continue
            for channel in payload.data:
                groups.setdefault(str(channel.epg_id), title)
        return groups

    async def hls_url(self, channel_id: str) -> HlsAddress:
        response = await self._request(
            "GET",
            HLS_PATH,
            params={"action": "getChannelHlsUrl", "channelId": channel_id},
            headers={"VIS-AJAX": "AjaxHttpRequest", "Accept": "*/*"},
        )
        payload = response.json()
        if not isinstance(payload, dict) or "hlsUrl" not in payload:
            raise IptvError("epg-stream", "response data is not an HLS address")
        return HlsAddress.model_validate(payload)

    async def programmes(self, query: ProgrammeQuery) -> list[Programme]:
        response = await self._request(
            "POST",
            CHANNEL_AJAX_PATH,
            data={
                "action": "getChannelProg",
                "code": query.channel_code,
                "channelID": query.channel_id,
                "endTime": str(query.end_time_ms),
                "startTime": str(query.start_time_ms),
                "offset": "0",
                "limit": "2000",
            },
            headers=AJAX_HEADERS,
        )
        payload = ProgrammeResponse.model_validate(response.json())
        if payload.error_code == "051002":
            return []
        if payload.status != "1":
            raise IptvError(
                "epg-programmes",
                payload.error_message or f"status={payload.status}",
            )
        return payload.data
    async def tvod_url(self, programme: Programme) -> TvodAddress:
        response = await self._request(
            "POST",
            CHANNEL_AJAX_PATH,
            data={
                "action": "getTvodPlayUrl",
                "channelID": str(programme.channel_id),
                "playbillID": programme.programme_id,
                "startTime": str(programme.start_time_ms // 1000),
                "endTime": str(programme.end_time_ms // 1000),
            },
            headers=AJAX_HEADERS,
        )
        payload = TvodAddressResponse.model_validate(response.json())
        if payload.status != "1":
            raise IptvError(
                "epg-tvod",
                payload.error_message or f"status={payload.status}",
            )
        return payload.data
