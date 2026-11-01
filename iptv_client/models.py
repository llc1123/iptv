from __future__ import annotations

from dataclasses import dataclass
from ipaddress import IPv4Address
from typing import NewType

from pydantic import BaseModel, ConfigDict, Field

ChannelId = NewType("ChannelId", str)
EpgChannelId = NewType("EpgChannelId", str)


@dataclass(frozen=True, slots=True)
class DhcpLease:
    address: IPv4Address
    router: IPv4Address
    server: IPv4Address
    subnet_mask: IPv4Address
    lease_seconds: int
    vendor_specific: bytes

    @property
    def prefix_length(self) -> int:
        return sum(value.bit_count() for value in self.subnet_mask.packed)


class Channel(BaseModel):
    model_config = ConfigDict(frozen=True, extra="ignore", populate_by_name=True)

    auth_code: str = Field(default="", alias="authCode")
    code: ChannelId = Field(alias="code")
    name: str
    is_charge: str = Field(default="", alias="isCharge")
    epg_id: EpgChannelId = Field(alias="ID")
    mix_no: str = Field(alias="mixNo")
    media_id: str = Field(default="", alias="mediaID")


class BootChannel(BaseModel):
    model_config = ConfigDict(frozen=True, extra="ignore", populate_by_name=True)

    channel_id: EpgChannelId = Field(alias="ChannelID")
    name: str = Field(alias="ChannelName")
    mix_no: str = Field(alias="UserChannelID")
    channel_url: str = Field(default="", alias="ChannelURL")
    time_shift_url: str = Field(alias="TimeShiftURL")
    channel_sdp: str = Field(default="", alias="ChannelSDP")
    time_shift: str = Field(default="", alias="TimeShift")
    time_shift_length: str = Field(default="", alias="TimeShiftLength")
    logo_url: str = Field(default="", alias="ChannelLogURL")
    code: ChannelId = Field(default=ChannelId(""), alias="code")
    group_title: str = ""


class Programme(BaseModel):
    model_config = ConfigDict(frozen=True, extra="ignore", populate_by_name=True)

    title: str = Field(alias="name")
    start_time_ms: int = Field(alias="startTime")
    end_time_ms: int = Field(alias="endTime")
    programme_id: str = Field(default="", alias="ID")
    channel_id: EpgChannelId = Field(alias="channelID")


class ProgrammeResponse(BaseModel):
    model_config = ConfigDict(frozen=True, extra="ignore", populate_by_name=True)

    data: list[Programme] = Field(default_factory=list)
    status: str
    error_code: str = Field(default="", alias="errCode")
    error_message: str = Field(default="", alias="errMsg")


class ChannelListResponse(BaseModel):
    model_config = ConfigDict(frozen=True, extra="ignore", populate_by_name=True)

    data: list[Channel] = Field(default_factory=list)
    status: str
    error_code: str = Field(default="", alias="errCode")
    error_message: str = Field(default="", alias="errMsg")


class TvodAddress(BaseModel):
    model_config = ConfigDict(
        frozen=True,
        extra="ignore",
        populate_by_name=True,
        coerce_numbers_to_str=True,
    )

    programme_id: str = Field(alias="ID")
    channel_id: EpgChannelId = Field(alias="channelID")
    start_time_ms: int = Field(alias="startTime")
    end_time_ms: int = Field(alias="endTime")
    play_url: str = Field(alias="playURL")


class TvodAddressResponse(BaseModel):
    model_config = ConfigDict(frozen=True, extra="ignore", populate_by_name=True)

    data: TvodAddress
    status: str
    error_code: str = Field(default="", alias="errCode")
    error_message: str = Field(default="", alias="errMsg")


class HlsAddress(BaseModel):
    model_config = ConfigDict(frozen=True, extra="ignore", populate_by_name=True)

    ts_time: str = Field(default="", alias="tsTime")
    is_ts: str = Field(default="", alias="isTs")
    status: str = ""
    hls_url: str = Field(alias="hlsUrl")
