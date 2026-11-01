from __future__ import annotations

import html
import re

from .errors import IptvError
from .models import BootChannel

_CHANNEL_START = re.compile(r'(?<![A-Za-z])ChannelID="')
_FIELD = re.compile(r'([A-Za-z]+)="([^"]*)"')


def parse_boot_channels(body: str) -> list[BootChannel]:
    starts = [match.start() for match in _CHANNEL_START.finditer(body)]
    channels: list[BootChannel] = []
    for index, start in enumerate(starts):
        end = starts[index + 1] if index + 1 < len(starts) else len(body)
        fields = {
            name: html.unescape(value.replace(r"\"", '"'))
            for name, value in _FIELD.findall(body[start:end])
        }
        required = {"ChannelID", "ChannelName", "UserChannelID", "TimeShiftURL"}
        if required.issubset(fields):
            channels.append(BootChannel.model_validate(fields))
    if not channels:
        raise IptvError(
            "epg-channels", "no boot channel records were found in the response"
        )
    return channels
