"""Failure status-shape golden cases.

Cases freeze the POC error-string format and the HTTP status mapping of
``web._handle_errors`` (web.py:186-194) for every POC operation code, plus
the provider response status rule used by ``epg.py``.
"""

from __future__ import annotations

from support import Case, error_expected

from iptv_client.errors import IptvError

_OPERATION_CODES = (
    "dhcp",
    "epg-auth",
    "epg-channel-codes",
    "epg-channels",
    "epg-discovery",
    "epg-http",
    "epg-programmes",
    "epg-replay",
    "epg-stream",
    "epg-tvod",
    "guide-cache",
    "hls-proxy",
    "logo",
    "logo-converter",
    "service",
)


def _http_status(operation: str) -> int:
    """Mirror web._handle_errors: hls-proxy is 502, everything else 503."""
    return 503 if operation != "hls-proxy" else 502


def status_shape_cases() -> list[Case]:
    """HTTP error mapping and provider status-shape cases."""
    cases = []
    for operation in _OPERATION_CODES:
        detail = f"fixture failure in {operation}"
        cases.append(
            Case(
                id=f"status-{operation}",
                covers=("failure-status-shapes",),
                input={"kind": "http-error", "operation": operation, "detail": detail},
                expected={
                    "status": _http_status(operation),
                    "body": {"error": str(IptvError(operation, detail))},
                },
            )
        )
    signed_detail = (
        "upstream GET http://upstream.example.com/live/chan.m3u8"
        "?token=fixture-token-00000000000001 failed with 503"
    )
    cases.append(
        Case(
            id="status-detail-with-signed-url-kept-by-poc",
            covers=("failure-status-shapes",),
            input={
                "kind": "http-error",
                "operation": "hls-proxy",
                "detail": signed_detail,
            },
            expected={
                "status": _http_status("hls-proxy"),
                "body": {"error": str(IptvError("hls-proxy", signed_detail))},
            },
        )
    )
    provider_scenarios = [
        ("provider-status-success", "epg-channel-codes", {"status": "1"}, None),
        (
            "provider-status-failure-err-message",
            "epg-channel-codes",
            {"status": "0", "errCode": "1001", "errMsg": "boom"},
            "boom",
        ),
        (
            "provider-status-failure-programmes",
            "epg-programmes",
            {"status": "2", "errCode": "1002", "errMsg": "window rejected"},
            "window rejected",
        ),
        (
            "provider-status-failure-fallback",
            "epg-tvod",
            {"status": "0", "errCode": "1003", "errMsg": ""},
            "status=0",
        ),
    ]
    for case_id, operation, payload, detail in provider_scenarios:
        expected: dict[str, object]
        if detail is None:
            expected = {"result": "ok"}
        else:
            expected = error_expected(
                "provider-status-failure", operation=operation, detail=detail
            )
        cases.append(
            Case(
                id=case_id,
                covers=("failure-status-shapes",),
                input={
                    "kind": "provider-status",
                    "operation": operation,
                    "payload": payload,
                },
                expected=expected,
            )
        )
    return cases
