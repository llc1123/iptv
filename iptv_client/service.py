from __future__ import annotations

import json
import time
from dataclasses import dataclass
from datetime import datetime, timedelta, tzinfo
from datetime import time as datetime_time
from pathlib import Path
from typing import Final
from zoneinfo import ZoneInfo

import anyio
import httpx2

from .credentials import (
    DEFAULT_DHCP_PASSWORD,
    DEFAULT_DHCP_USER,
    DEFAULT_EPG_PASSWORD,
    DEFAULT_EPG_USER,
)
from .dhcp import acquire_lease, configure_interface, interface_mac
from .epg import EDS_AUTH_URL, EpgClient, EpgConfig, ProgrammeQuery
from .errors import IptvError
from .logo_catalog import (
    DEFAULT_CATALOG_PATH,
    DEFAULT_CONVERTER_URL,
    LogoCatalog,
    LogoCatalogBuilder,
)
from .models import BootChannel, Programme
from .xmltv import render_m3u, render_xmltv

DEFAULT_GUIDE_TIMEZONE: Final = "Asia/Shanghai"
DEFAULT_GUIDE_CACHE_PATH: Final = Path("var/logo-catalog/guide.json")
DEFAULT_AUTH_REFRESH_SECONDS: Final = 600.0
GUIDE_CONCURRENCY: Final = 8


@dataclass(frozen=True, slots=True)
class ServiceConfig:
    interface: str = "eth1"
    dhcp_user: str = DEFAULT_DHCP_USER
    dhcp_password: str = DEFAULT_DHCP_PASSWORD
    epg_user: str = DEFAULT_EPG_USER
    epg_password: str = DEFAULT_EPG_PASSWORD
    epg_mac: str | None = None
    stb_id: str = ""
    authentication_url: str = EDS_AUTH_URL
    guide_past_days: int = 7
    guide_future_days: int = 2
    guide_timezone: str = DEFAULT_GUIDE_TIMEZONE
    guide_cache_path: Path = DEFAULT_GUIDE_CACHE_PATH
    logo_catalog_path: Path = DEFAULT_CATALOG_PATH
    metadata_converter_url: str = DEFAULT_CONVERTER_URL


def _day_ranges(
    now: datetime, past_days: int, future_days: int
) -> list[tuple[int, int]]:
    first_day = now.date() - timedelta(days=past_days)
    last_day = now.date() + timedelta(days=future_days)
    ranges: list[tuple[int, int]] = []
    day = first_day
    while day <= last_day:
        start = datetime.combine(day, datetime_time.min, now.tzinfo)
        end = start + timedelta(days=1)
        ranges.append((int(start.timestamp() * 1000), int(end.timestamp() * 1000) - 1))
        day += timedelta(days=1)
    return ranges


class IptvService:
    def __init__(self, config: ServiceConfig) -> None:
        self._config = config
        self._timezone: tzinfo = ZoneInfo(config.guide_timezone)
        self._epg: EpgClient | None = None
        self._channels: tuple[BootChannel, ...] = ()
        self._authenticated_at = 0.0
        self._refresh_lock = anyio.Lock()
        self._guide_lock = anyio.Lock()
        self._guide_cache: bytes | None = None
        self._guide_cache_base_url = ""
        self._guide_programmes = self._load_guide_cache(config.guide_cache_path)
        self._logo_catalog = self._load_logo_catalog(config.logo_catalog_path)
        self._metadata_sync_lock = anyio.Lock()

    async def start(self) -> None:
        lease = await anyio.to_thread.run_sync(
            acquire_lease,
            self._config.interface,
            10.0,
            self._config.dhcp_password,
            self._config.dhcp_user,
        )
        configure_interface(self._config.interface, lease)
        epg_mac = self._config.epg_mac or interface_mac(self._config.interface)
        self._epg = EpgClient(
            EpgConfig(
                user_id=self._config.epg_user,
                stb_id=self._config.stb_id,
                stb_mac=epg_mac,
                password=self._config.epg_password,
                authentication_url=self._config.authentication_url,
            ),
            lease.address,
        )
        await self._epg.__aenter__()
        await self._refresh()

    async def close(self) -> None:
        if self._epg is not None:
            await self._epg.__aexit__(None, None, None)
            self._epg = None

    async def _refresh(self) -> None:
        if self._epg is None:
            raise IptvError("service", "service is not started")
        epg = self._epg
        await epg.authenticate()
        self._channels = tuple(await epg.channels())
        self._authenticated_at = time.monotonic()
        self._guide_cache = None
        self._guide_cache_base_url = ""

    @staticmethod
    def _load_logo_catalog(path: Path) -> LogoCatalog | None:
        if not path.is_file():
            return None
        return LogoCatalog.load(path)

    def _render_channels(self, base_url: str) -> tuple[BootChannel, ...]:
        if self._logo_catalog is None:
            return tuple(
                channel.model_copy(update={"logo_url": ""})
                for channel in self._channels
            )
        return tuple(
            channel.model_copy(
                update={
                    "logo_url": self._logo_catalog.logo_url(
                        str(channel.channel_id), base_url
                    )
                    or ""
                }
            )
            for channel in self._channels
        )

    async def _ensure_ready(self) -> None:
        if self._epg is None:
            raise IptvError("service", "service is not started")
        if time.monotonic() - self._authenticated_at < DEFAULT_AUTH_REFRESH_SECONDS:
            return
        async with self._refresh_lock:
            if (
                time.monotonic() - self._authenticated_at
                >= DEFAULT_AUTH_REFRESH_SECONDS
            ):
                await self._refresh()

    @property
    def channels(self) -> tuple[BootChannel, ...]:
        return self._channels

    @property
    def http_client(self) -> httpx2.AsyncClient:
        if self._epg is None:
            raise IptvError("service", "service is not started")
        return self._epg.http_client

    def channel(self, channel_id: str) -> BootChannel:
        for channel in self._channels:
            if channel.channel_id == channel_id:
                return channel
        raise IptvError("service", f"unknown channel {channel_id}")

    async def playlist(self, base_url: str) -> str:
        await self._ensure_ready()
        return render_m3u(
            self._render_channels(base_url),
            base_url,
            # The guide covers complete calendar days, so the first day can
            # be almost one extra day older than the configured 24-hour span.
            catchup_days=self._config.guide_past_days + 1,
        )

    async def upstream_hls_url(self, channel_id: str) -> str:
        await self._ensure_ready()
        if self._epg is None:
            raise IptvError("service", "service is not started")
        channel = self.channel(channel_id)
        if channel.time_shift != "1":
            raise IptvError("epg-tvod", f"channel {channel_id} has no replay")
        return (await self._epg.hls_url(channel_id)).hls_url

    async def replay_hls_url(self, channel_id: str, programme_id: str) -> str:
        await self._ensure_ready()
        if self._epg is None:
            raise IptvError("service", "service is not started")
        self.channel(channel_id)
        programme = next(
            (
                item
                for item in self._guide_programmes or ()
                if str(item.channel_id) == channel_id
                and item.programme_id == programme_id
            ),
            None,
        )
        if programme is None:
            raise IptvError("epg-replay", f"unknown programme {programme_id}")
        return (await self._epg.tvod_url(programme)).play_url

    async def replay_hls_url_for_range(
        self, channel_id: str, start_time_ms: int
    ) -> str:
        await self._ensure_ready()
        if self._epg is None:
            raise IptvError("service", "service is not started")
        self.channel(channel_id)
        programme = next(
            (
                item
                for item in self._guide_programmes or ()
                if str(item.channel_id) == channel_id
                and item.start_time_ms <= start_time_ms < item.end_time_ms
            ),
            None,
        )
        if programme is None:
            raise IptvError(
                "epg-replay", f"no programme covers timestamp {start_time_ms}"
            )
        return (await self._epg.tvod_url(programme)).play_url

    async def guide(self, base_url: str) -> bytes:
        await self._ensure_ready()
        if self._guide_cache is not None and self._guide_cache_base_url == base_url:
            return self._guide_cache
        async with self._guide_lock:
            if self._guide_cache is not None and self._guide_cache_base_url == base_url:
                return self._guide_cache
            guide = render_xmltv(
                self._render_channels(base_url),
                self._guide_programmes or (),
                timezone=self._timezone,
                base_url=base_url,
            )
            self._guide_cache = guide
            self._guide_cache_base_url = base_url
            return guide

    def logo_path(self, channel_id: str) -> Path | None:
        if self._logo_catalog is None:
            return None
        return self._logo_catalog.logo_path(channel_id)

    async def sync_metadata(self) -> dict[str, int | str]:
        if self._epg is None:
            raise IptvError("service", "service is not started")
        async with self._metadata_sync_lock:
            await self._refresh()
            catalog = await LogoCatalogBuilder(
                converter_url=self._config.metadata_converter_url,
            ).build(self._channels, self._config.logo_catalog_path)
            self._logo_catalog = catalog
            programmes = await self._fetch_programmes()
            self._write_guide_cache(programmes)
            self._guide_programmes = programmes
            self._guide_cache = None
            self._guide_cache_base_url = ""
            return {
                "catalog": str(self._config.logo_catalog_path),
                "guide_cache": str(self._config.guide_cache_path),
                "channels": len(catalog.entries),
                "logos": sum(
                    bool(entry.logo_path) for entry in catalog.entries.values()
                ),
                "guide_programmes": len(programmes),
            }

    @staticmethod
    def _load_guide_cache(path: Path) -> list[Programme] | None:
        if not path.is_file():
            return None
        try:
            payload = json.loads(path.read_text(encoding="utf-8"))
            if not isinstance(payload, list):
                raise TypeError("guide cache must contain a JSON list")
            return [Programme.model_validate(item) for item in payload]
        except (OSError, TypeError, ValueError) as error:
            raise IptvError("guide-cache", str(error)) from error

    def _write_guide_cache(self, programmes: list[Programme]) -> None:
        path = self._config.guide_cache_path
        path.parent.mkdir(parents=True, exist_ok=True)
        temporary_path = path.with_suffix(f"{path.suffix}.tmp")
        temporary_path.write_text(
            json.dumps(
                [programme.model_dump(by_alias=True) for programme in programmes],
                ensure_ascii=False,
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )
        temporary_path.replace(path)

    async def _fetch_programmes(self) -> list[Programme]:
        if self._epg is None:
            raise IptvError("service", "service is not started")
        epg = self._epg
        ranges = _day_ranges(
            datetime.now(self._timezone),
            self._config.guide_past_days,
            self._config.guide_future_days,
        )
        limiter = anyio.CapacityLimiter(GUIDE_CONCURRENCY)
        results: list[Programme] = []
        requests = [
            ProgrammeQuery(
                channel_id=str(channel.channel_id),
                channel_code=str(channel.code),
                start_time_ms=start_ms,
                end_time_ms=end_ms,
            )
            for channel in self._channels
            if channel.code
            for start_ms, end_ms in ranges
        ]

        async def fetch(query: ProgrammeQuery) -> None:
            async with limiter:
                try:
                    results.extend(await epg.programmes(query))
                except IptvError:
                    return

        async with anyio.create_task_group() as task_group:
            for query in requests:
                task_group.start_soon(fetch, query)
        return results
