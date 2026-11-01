from __future__ import annotations

from collections.abc import Iterable
from datetime import datetime, tzinfo
from typing import Final
from urllib.parse import quote
from xml.etree import ElementTree

from .models import BootChannel, Programme

XMLTV_TIME_FORMAT: Final = "%Y%m%d%H%M%S %z"


def _xmltv_time(timestamp_ms: int, timezone: tzinfo) -> str:
    return datetime.fromtimestamp(timestamp_ms / 1000, timezone).strftime(
        XMLTV_TIME_FORMAT
    )


def render_xmltv(
    channels: Iterable[BootChannel],
    programmes: Iterable[Programme],
    *,
    timezone: tzinfo,
    base_url: str,
) -> bytes:
    root = ElementTree.Element("tv", {"generator-info-name": "iptv-media-server"})
    channel_list = list(channels)
    for channel in channel_list:
        node = ElementTree.SubElement(root, "channel", {"id": channel.channel_id})
        ElementTree.SubElement(node, "display-name").text = channel.name
        if channel.logo_url:
            ElementTree.SubElement(node, "icon", {"src": channel.logo_url})

    seen: set[tuple[str, int, int, str]] = set()
    channel_ids = {channel.channel_id for channel in channel_list}
    for programme in sorted(
        programmes, key=lambda item: (item.channel_id, item.start_time_ms)
    ):
        key = (
            programme.channel_id,
            programme.start_time_ms,
            programme.end_time_ms,
            programme.title,
        )
        if programme.channel_id not in channel_ids or key in seen:
            continue
        seen.add(key)
        node = ElementTree.SubElement(
            root,
            "programme",
            {
                "start": _xmltv_time(programme.start_time_ms, timezone),
                "stop": _xmltv_time(programme.end_time_ms, timezone),
                "channel": programme.channel_id,
            },
        )
        ElementTree.SubElement(node, "title").text = programme.title
        if programme.programme_id:
            ElementTree.SubElement(
                node, "episode-num", {"system": "iptv-id"}
            ).text = programme.programme_id
            ElementTree.SubElement(node, "url").text = (
                f"{base_url.rstrip('/')}/replay/"
                f"{quote(programme.channel_id, safe='')}/"
                f"{quote(programme.programme_id, safe='')}.m3u8"
            )

    ElementTree.indent(root, space="  ")
    return ElementTree.tostring(root, encoding="utf-8", xml_declaration=True)


def render_m3u(
    channels: Iterable[BootChannel],
    base_url: str,
    *,
    catchup_days: int = 0,
) -> str:
    guide_url = f"{base_url.rstrip('/')}/xmltv.xml"
    lines = [
        f'#EXTM3U x-tvg-url="{guide_url}"',
    ]
    for channel in channels:
        attributes = [
            f'tvg-id="{channel.channel_id}"',
            f'tvg-name="{channel.name}"',
        ]
        if channel.group_title:
            attributes.append(f'group-title="{channel.group_title}"')
        if channel.time_shift == "1" and catchup_days > 0:
            channel_id = quote(channel.channel_id, safe="")
            attributes.extend(
                (
                    'catchup="append"',
                    f'catchup-days="{catchup_days}"',
                    f'catchup-source="{base_url.rstrip("/")}/catchup/{channel_id}.m3u8?utc={{utc}}&lutc={{lutc}}"',
                )
            )
        if channel.logo_url:
            attributes.append(f'tvg-logo="{channel.logo_url}"')
        lines.append(f"#EXTINF:-1 {' '.join(attributes)},{channel.name}")
        channel_id = quote(channel.channel_id, safe="")
        lines.append(f"{base_url}/stream/{channel_id}.m3u8")
    return "\n".join(lines) + "\n"
