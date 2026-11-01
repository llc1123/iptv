from __future__ import annotations

import hashlib
import json
import mimetypes
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Final
from urllib.parse import quote, urlsplit

import anyio
import httpx2
from pydantic import BaseModel, ConfigDict, Field

from .errors import IptvError
from .models import BootChannel

DEFAULT_CONVERTER_URL: Final = "https://epg.51zmt.top:8001"
DEFAULT_CATALOG_PATH: Final = Path("var/logo-catalog/catalog.json")
_ATTRIBUTE = re.compile(r'([A-Za-z][A-Za-z0-9_-]*)="([^"]*)"')
_HTML_INPUT = re.compile(r"<input\b[^>]*>", re.IGNORECASE)
_HTML_ATTRIBUTE = re.compile(r"([A-Za-z_:][A-Za-z0-9_.:-]*)\s*=\s*[\"']([^\"']*)")
_IMAGE_EXTENSIONS = {".gif", ".jpeg", ".jpg", ".png", ".svg", ".webp"}


class LogoMetadata(BaseModel):
    model_config = ConfigDict(frozen=True, extra="ignore")

    channel_id: str
    stream_url: str
    display_name: str
    tvg_id: str = ""
    tvg_name: str = ""
    tvg_logo: str = ""
    group_title: str = ""
    logo_path: str = ""


class LogoCatalogFile(BaseModel):
    model_config = ConfigDict(frozen=True, extra="ignore")

    version: int = 1
    raw_m3u: str = "channels.raw.m3u"
    matched_m3u: str = "channels.matched.m3u"
    channels: list[LogoMetadata] = Field(default_factory=list)


@dataclass(frozen=True, slots=True)
class LogoCatalog:
    root: Path
    entries: dict[str, LogoMetadata]

    @classmethod
    def load(cls, path: Path) -> LogoCatalog:
        catalog = LogoCatalogFile.model_validate_json(path.read_bytes())
        root = path.parent.resolve()
        return cls(root, {item.channel_id: item for item in catalog.channels})

    def logo_path(self, channel_id: str) -> Path | None:
        entry = self.entries.get(channel_id)
        if entry is None or not entry.logo_path:
            return None
        path = (self.root / entry.logo_path).resolve()
        if self.root not in path.parents or not path.is_file():
            return None
        return path

    def logo_url(self, channel_id: str, base_url: str) -> str | None:
        if self.logo_path(channel_id) is None:
            return None
        return f"{base_url.rstrip('/')}/logos/{quote(channel_id, safe='')}"


@dataclass(frozen=True, slots=True)
class LogoCatalogBuilder:
    converter_url: str = DEFAULT_CONVERTER_URL
    timeout_seconds: float = 60.0

    async def build(
        self,
        channels: tuple[BootChannel, ...],
        output_path: Path,
        *,
        m3u_base_url: str = "http://127.0.0.1:8080",
    ) -> LogoCatalog:
        output_path = output_path.resolve()
        output_path.parent.mkdir(parents=True, exist_ok=True)
        raw_text = render_raw_m3u(channels, m3u_base_url)
        matched_text = await self._convert(raw_text)
        entries = _merge_metadata(channels, parse_m3u_metadata(matched_text))
        async with httpx2.AsyncClient(
            follow_redirects=True,
            timeout=self.timeout_seconds,
            trust_env=False,
        ) as client:
            entries = await self._download_logos(client, output_path.parent, entries)

        catalog = LogoCatalogFile(channels=entries)
        _write_text(output_path.parent / catalog.raw_m3u, raw_text)
        _write_text(output_path.parent / catalog.matched_m3u, matched_text)
        _write_json(output_path, catalog.model_dump_json(indent=2) + "\n")
        return LogoCatalog.load(output_path)

    async def _convert(self, raw_text: str) -> str:
        url = f"{self.converter_url.rstrip('/')}/upload/"
        timeout = httpx2.Timeout(self.timeout_seconds)
        async with httpx2.AsyncClient(
            follow_redirects=True, timeout=timeout, trust_env=False
        ) as client:
            try:
                page = await client.get(self.converter_url.rstrip("/") + "/")
                page.raise_for_status()
                token = _csrf_token(page.text, client.cookies)
                data = {"csrfmiddlewaretoken": token} if token else {}
                response = await client.post(
                    url,
                    data=data,
                    files={
                        "m3u_file": (
                            "channels.m3u",
                            raw_text.encode("utf-8"),
                            "audio/x-mpegurl",
                        )
                    },
                    headers={"Referer": str(page.url)},
                )
                response.raise_for_status()
                payload = response.json()
                if not isinstance(payload, dict):
                    raise IptvError(
                        "logo-converter", "upload response was not an object"
                    )
                filename = payload.get("output_filename") or payload.get("filename")
                if not isinstance(filename, str) or not filename:
                    raise IptvError(
                        "logo-converter", "upload response omitted output_filename"
                    )
                download_url = (
                    f"{self.converter_url.rstrip('/')}/download/"
                    f"{quote(filename, safe='')}/"
                )
                response = await client.get(download_url)
                response.raise_for_status()
                return response.text
            except (httpx2.HTTPError, json.JSONDecodeError) as error:
                raise IptvError("logo-converter", str(error)) from error

    async def _download_logos(
        self,
        client: httpx2.AsyncClient,
        root: Path,
        entries: list[LogoMetadata],
    ) -> list[LogoMetadata]:
        logo_root = root / "logos"
        logo_root.mkdir(parents=True, exist_ok=True)
        downloaded: dict[str, str] = {}
        result: list[LogoMetadata] = []
        for entry in entries:
            if not entry.tvg_logo:
                result.append(entry)
                continue
            if entry.tvg_logo in downloaded:
                result.append(
                    entry.model_copy(update={"logo_path": downloaded[entry.tvg_logo]})
                )
                continue
            try:
                response = await client.get(entry.tvg_logo)
                response.raise_for_status()
            except httpx2.HTTPError:
                result.append(entry)
                continue
            extension = _image_extension(
                entry.tvg_logo, response.headers.get("content-type")
            )
            relative_path = f"logos/{_safe_filename(entry.channel_id)}{extension}"
            path = root / relative_path
            await anyio.Path(path).write_bytes(response.content)
            downloaded[entry.tvg_logo] = relative_path
            result.append(entry.model_copy(update={"logo_path": relative_path}))
        return result


def render_raw_m3u(channels: tuple[BootChannel, ...], base_url: str) -> str:
    lines = ["#EXTM3U"]
    for channel in channels:
        channel_id = quote(str(channel.channel_id), safe="")
        lines.append(
            f'#EXTINF:-1 tvg-id="{channel.channel_id}" tvg-name="{channel.name}",'
            f"{channel.name}"
        )
        lines.append(f"{base_url.rstrip('/')}/stream/{channel_id}.m3u8")
    return "\n".join(lines) + "\n"


def parse_m3u_metadata(text: str) -> list[LogoMetadata]:
    entries: list[LogoMetadata] = []
    pending: dict[str, str] | None = None
    for line in text.splitlines():
        if line.startswith("#EXTINF:"):
            attributes = dict(_ATTRIBUTE.findall(line))
            display_name = line.rsplit(",", 1)[-1].strip()
            pending = {
                "display_name": display_name,
                "tvg_id": attributes.get("tvg-id", ""),
                "tvg_name": attributes.get("tvg-name", ""),
                "tvg_logo": attributes.get("tvg-logo", ""),
                "group_title": attributes.get("group-title", ""),
            }
            continue
        if pending is None or not line or line.startswith("#"):
            continue
        parsed = urlsplit(line.strip())
        path_parts = [part for part in parsed.path.split("/") if part]
        if not path_parts:
            pending = None
            continue
        channel_id = path_parts[-1].removesuffix(".m3u8")
        entries.append(
            LogoMetadata(
                channel_id=channel_id,
                stream_url=line.strip(),
                **pending,
            )
        )
        pending = None
    return entries


def _merge_metadata(
    channels: tuple[BootChannel, ...], matched: list[LogoMetadata]
) -> list[LogoMetadata]:
    by_id = {entry.channel_id: entry for entry in matched}
    by_name: dict[str, list[LogoMetadata]] = {}
    for entry in matched:
        by_name.setdefault(_normalise_name(entry.display_name), []).append(entry)
        if entry.tvg_name:
            by_name.setdefault(_normalise_name(entry.tvg_name), []).append(entry)

    used: set[int] = set()
    merged: list[LogoMetadata] = []
    for channel in channels:
        entry = by_id.get(str(channel.channel_id))
        if entry is None:
            candidates = by_name.get(_normalise_name(channel.name), [])
            entry = next(
                (candidate for candidate in candidates if id(candidate) not in used),
                None,
            )
        if entry is not None:
            used.add(id(entry))
            merged.append(
                entry.model_copy(update={"channel_id": str(channel.channel_id)})
            )
            continue
        merged.append(
            LogoMetadata(
                channel_id=str(channel.channel_id),
                stream_url=channel.channel_url,
                display_name=channel.name,
                tvg_id=str(channel.channel_id),
                tvg_name=channel.name,
            )
        )
    return merged


def _csrf_token(page: str, cookies: httpx2.Cookies) -> str:
    for tag in _HTML_INPUT.findall(page):
        attributes = {
            name.casefold(): value for name, value in _HTML_ATTRIBUTE.findall(tag)
        }
        name = attributes.get("name", "").casefold()
        if name in {"csrfmiddlewaretoken", "csrf_token", "_csrf"}:
            return attributes.get("value", "")
    for name in ("csrftoken", "csrf_token", "_csrf"):
        value = cookies.get(name)
        if value:
            return value
    return ""


def _image_extension(url: str, content_type: str | None) -> str:
    mime = (content_type or "").split(";", 1)[0].strip().lower()
    extension = mimetypes.guess_extension(mime) if mime else None
    if extension == ".jpe":
        extension = ".jpg"
    if extension in _IMAGE_EXTENSIONS:
        return extension
    suffix = Path(urlsplit(url).path).suffix.lower()
    return suffix if suffix in _IMAGE_EXTENSIONS else ".img"


def _safe_filename(value: str) -> str:
    cleaned = re.sub(r"[^A-Za-z0-9_.-]+", "_", value).strip("._")
    return cleaned or hashlib.sha256(value.encode("utf-8")).hexdigest()[:16]


def _normalise_name(value: str) -> str:
    return re.sub(r"\s+", "", value).casefold()


def _write_text(path: Path, text: str) -> None:
    path.write_text(text, encoding="utf-8")


def _write_json(path: Path, text: str) -> None:
    path.write_text(text, encoding="utf-8")
