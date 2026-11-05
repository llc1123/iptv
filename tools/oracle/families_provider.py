"""Provider parser, model, and failure-status golden cases.

Expected values come from executing ``iptv_client`` parsing and validation
code against fixed bodies and payloads; error strings and status mappings
mirror ``web._handle_errors`` (web.py:186-194) and ``errors.IptvError``.
"""

from __future__ import annotations

from pydantic import BaseModel, ValidationError
from support import Case, error_expected

from iptv_client.models import (
    BootChannel,
    Channel,
    ChannelListResponse,
    HlsAddress,
    Programme,
    ProgrammeResponse,
    TvodAddress,
)
from iptv_client.parsers import parse_boot_channels


def _parser_case(
    case_id: str, body: str, *, covers: tuple[str, ...] = ("parsers",)
) -> Case:
    channels = parse_boot_channels(body)
    return Case(
        id=case_id,
        covers=covers,
        input={"body": body},
        expected={"channels": [channel.model_dump() for channel in channels]},
    )


def parser_cases() -> list[Case]:
    """Boot-channel parser cases including duplicate and entity quirks."""
    return [
        _parser_case(
            "parsers-multi-record",
            (
                'payload ChannelID="c1" ChannelName="CCTV-1" UserChannelID="1" '
                'TimeShiftURL="http://upstream.example.com/ts/c1" ChannelURL="rtsp://c1" '
                'ChannelSDP="sdp1" TimeShift="1" TimeShiftLength="7200" '
                'ChannelLogURL="http://upstream.example.com/logo/c1.png" code="cc1";'
                ' ChannelID="c2" ChannelName="CCTV-2" UserChannelID="2" '
                'TimeShiftURL="http://upstream.example.com/ts/c2"'
            ),
        ),
        _parser_case(
            "parsers-duplicate-attributes-last-wins",
            (
                'x ChannelID="c1" ChannelName="first" ChannelName="second" '
                'UserChannelID="7" TimeShiftURL="t1"'
            ),
            covers=("parsers", "duplicate-attributes"),
        ),
        _parser_case(
            "parsers-html-entities",
            (
                'x ChannelID="c1" ChannelName="News &amp; Sports &lt;Live&gt; &quot;Q&quot; &amp;amp;" '
                'UserChannelID="7" TimeShiftURL="t1"'
            ),
            covers=("parsers", "html-entities"),
        ),
        _parser_case(
            "parsers-escaped-quote-truncates-value",
            'x ChannelID="c1" ChannelName="A \\"B\\" tail" UserChannelID="7" TimeShiftURL="t1"',
        ),
        _parser_case(
            "parsers-fields-before-first-record-ignored",
            (
                'UserChannelID="junk" ChannelName="junk" ChannelID="c2" '
                'ChannelName="n2" UserChannelID="7" TimeShiftURL="t2"'
            ),
        ),
        _parser_case(
            "parsers-missing-required-skips-record",
            (
                'x ChannelID="bad" ChannelName="no-timeshift" UserChannelID="1";'
                ' ChannelID="good" ChannelName="ok" UserChannelID="2" TimeShiftURL="t2"'
            ),
        ),
        Case(
            id="parsers-no-records-error",
            covers=("parsers",),
            input={"body": "no channel markup at all"},
            expected=error_expected(
                "parsers-no-boot-channels",
                operation="epg-channels",
                detail="no boot channel records were found in the response",
            ),
        ),
    ]


def _model_case(
    case_id: str,
    validator: type[BaseModel],
    payload: dict[str, object],
    *,
    covers: tuple[str, ...] = ("models",),
) -> Case:
    try:
        parsed = validator.model_validate(payload)
    except ValidationError:
        return Case(
            id=case_id,
            covers=covers,
            input={"model": validator.__name__, "payload": payload},
            expected=error_expected("models-validation", operation="epg-http"),
        )
    fields = parsed.model_dump()
    return Case(
        id=case_id,
        covers=covers,
        input={"model": validator.__name__, "payload": payload},
        expected={"fields": fields},
    )


def model_cases() -> list[Case]:
    """Provider model mapping cases including coercion quirks."""
    return [
        _model_case(
            "models-channel-alias-mapping",
            Channel,
            {
                "authCode": "auth-1",
                "code": "cc1",
                "name": "CCTV-1",
                "isCharge": "0",
                "ID": "epg-1",
                "mixNo": "7",
                "mediaID": "media-1",
                "ignored_extra": "dropped",
            },
        ),
        _model_case(
            "models-channel-missing-required",
            Channel,
            {"name": "CCTV-1", "ID": "epg-1", "mixNo": "7"},
        ),
        _model_case(
            "models-channel-numeric-fields-rejected",
            Channel,
            {"code": 5, "name": "CCTV-1", "ID": "epg-1", "mixNo": 7},
        ),
        _model_case(
            "models-programme-string-times-coerced",
            Programme,
            {
                "name": "News",
                "startTime": "1699971200000",
                "endTime": 1699974800000,
                "channelID": "epg-1",
            },
        ),
        _model_case(
            "models-tvod-coerces-numeric-id",
            TvodAddress,
            {
                "ID": 12345,
                "channelID": "epg-1",
                "startTime": 1699971200000,
                "endTime": 1699974800000,
                "playURL": "http://upstream.example.com/vod/12345.m3u8",
            },
        ),
        _model_case(
            "models-hls-address-defaults",
            HlsAddress,
            {"hlsUrl": "http://upstream.example.com/live/c1.m3u8"},
        ),
        _model_case(
            "models-hls-address-numeric-tstime-rejected",
            HlsAddress,
            {"hlsUrl": "http://upstream.example.com/live/c1.m3u8", "tsTime": 5},
        ),
        _model_case(
            "models-boot-channel-group-title",
            BootChannel,
            {
                "ChannelID": "c1",
                "ChannelName": "CCTV-1",
                "UserChannelID": "1",
                "TimeShiftURL": "http://upstream.example.com/ts/c1",
                "group_title": "News",
                "code": "cc1",
            },
        ),
        _model_case(
            "models-programme-response-mapping",
            ProgrammeResponse,
            {
                "data": [
                    {
                        "name": "News",
                        "startTime": 1699971200000,
                        "endTime": 1699974800000,
                        "ID": "prog-1",
                        "channelID": "epg-1",
                    }
                ],
                "status": "0",
                "errCode": "1001",
                "errMsg": "boom",
            },
        ),
        _model_case(
            "models-channel-list-response-mapping",
            ChannelListResponse,
            {
                "data": [
                    {
                        "authCode": "auth-1",
                        "code": "cc1",
                        "name": "CCTV-1",
                        "isCharge": "0",
                        "ID": "epg-1",
                        "mixNo": "7",
                        "mediaID": "media-1",
                    }
                ],
                "status": "1",
            },
        ),
    ]
