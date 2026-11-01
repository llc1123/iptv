"""Option 60, EPG authenticator, and padding golden cases.

All expected values are produced by executing the Python POC crypto modules
with injected randomness and timestamps; no expected bytes are hand-written.
"""

from __future__ import annotations

from hashlib import md5
from typing import Final

from Crypto.Cipher import DES
from Crypto.Util.Padding import pad
from support import (
    FIXTURE_CUSTOM_STR,
    FIXTURE_DHCP_PASSWORD,
    FIXTURE_DHCP_USER,
    FIXTURE_ENCRYPT_TOKEN,
    FIXTURE_EPG_PASSWORD,
    FIXTURE_EPG_USER,
    FIXTURE_IP_ADDRESS,
    FIXTURE_MAC,
    FIXTURE_R_HEX,
    FIXTURE_RANDOM_VALUE,
    FIXTURE_STB_ID,
    FIXTURE_TS_HEX,
    Case,
    error_expected,
)

from iptv_client import ctc_dhcp
from iptv_client.auth import make_authenticator

_LOGIN_ALIGNED: Final = "12345678"
_SESSION_R: Final = bytes.fromhex(FIXTURE_R_HEX)
_SESSION_TS: Final = bytes.fromhex(FIXTURE_TS_HEX)


def _wire_parts(wire: bytes) -> dict[str, object]:
    """Split a wire image into the fields the wire layout declares."""
    return {
        "wire_hex": wire.hex(),
        "declared_length": wire[3],
        "hash_key_hex": wire[21:37].hex(),
        "ciphertext_hex": wire[37:].hex(),
    }


def _encode_case(
    case_id: str,
    *,
    login: str,
    password: str = FIXTURE_DHCP_PASSWORD,
    r: bytes = _SESSION_R,
    ts: bytes = _SESSION_TS,
    covers: tuple[str, ...] = ("option60",),
) -> Case:
    wire = ctc_dhcp.make_option60(password=password, login=login, r=r, ts=ts)
    expected = _wire_parts(wire)
    expected["hash_key_hex"] = md5(r + password.encode() + ts).hexdigest()
    return Case(
        id=case_id,
        covers=covers,
        input={
            "direction": "encode",
            "login": login,
            "password": password,
            "r_hex": r.hex(),
            "ts_hex": ts.hex(),
        },
        expected=expected,
    )


def _decode_reject_case(
    case_id: str,
    *,
    wire: bytes,
    kind: str,
    violates: str,
    covers: tuple[str, ...] = ("option60",),
) -> Case:
    return Case(
        id=case_id,
        covers=covers,
        input={"direction": "decode", "violates": violates, "wire_hex": wire.hex()},
        expected=error_expected(kind, operation="dhcp"),
    )


def option60_cases() -> list[Case]:
    """Option 60 encode known-answers and decode rejection vectors."""
    base = ctc_dhcp.make_option60(
        password=FIXTURE_DHCP_PASSWORD,
        login=FIXTURE_DHCP_USER,
        r=_SESSION_R,
        ts=_SESSION_TS,
    )
    return [
        _encode_case("option60-known-answer", login=FIXTURE_DHCP_USER),
        _encode_case(
            "option60-second-vector",
            login=_LOGIN_ALIGNED,
            password="short",
            r=b"\x01\x00\x00\x00\x00\x00\x00\x00",
            ts=b"\x2a\x00\x00\x00\x00\x00\x00\x00",
        ),
        _encode_case(
            "option60-zero-pad-aligned",
            login=_LOGIN_ALIGNED,
            covers=("option60", "aligned-padding"),
        ),
        _encode_case(
            "option60-zero-pad-non-aligned",
            login="fixture-user",
            covers=("option60", "non-aligned-padding"),
        ),
        _encode_case(
            "option60-zero-pad-empty-login",
            login="",
            covers=("option60", "aligned-padding"),
        ),
        _encode_case("option60-near-length-limit", login="x" * 212),
        Case(
            id="option60-oversized-login",
            covers=("option60", "malformed-lengths"),
            input={
                "direction": "encode",
                "login": "x" * 220,
                "password": FIXTURE_DHCP_PASSWORD,
                "r_hex": FIXTURE_R_HEX,
                "ts_hex": FIXTURE_TS_HEX,
            },
            expected=error_expected(
                "option60-oversized-payload",
                operation="dhcp",
                detail="CTC Option 60 exceeds the one-byte length field",
            ),
        ),
        _decode_reject_case(
            "option60-reject-bad-prefix",
            wire=b"\x00\x00\x1e" + base[3:],
            kind="option60-malformed-prefix",
            violates="prefix",
        ),
        _decode_reject_case(
            "option60-reject-length-mismatch",
            wire=base[:3] + bytes([base[3] + 1]) + base[4:],
            kind="option60-malformed-length",
            violates="length",
            covers=("option60", "malformed-lengths"),
        ),
        _decode_reject_case(
            "option60-reject-truncated",
            wire=base[:-8],
            kind="option60-malformed-length",
            violates="length",
            covers=("option60", "malformed-lengths"),
        ),
        _decode_reject_case(
            "option60-reject-bad-method",
            wire=base[:4] + b"\x02" + base[5:],
            kind="option60-malformed-method",
            violates="method",
        ),
    ]


def epg_auth_cases() -> list[Case]:
    """EPG authenticator known-answers across key-padding and nonce edges."""
    scenarios = [
        ("epg-auth-known-answer", FIXTURE_RANDOM_VALUE, FIXTURE_EPG_PASSWORD),
        ("epg-auth-key-pad-empty-password", FIXTURE_RANDOM_VALUE, ""),
        ("epg-auth-key-pad-8", FIXTURE_RANDOM_VALUE, "12345678"),
        ("epg-auth-key-pad-24", FIXTURE_RANDOM_VALUE, "fixture-password-24chars"),
        (
            "epg-auth-key-truncates-over-24",
            FIXTURE_RANDOM_VALUE,
            "fixture-password-24chars-overlong",
        ),
        ("epg-auth-random-zero", 0, FIXTURE_EPG_PASSWORD),
        ("epg-auth-random-boundary", 99_999_999, FIXTURE_EPG_PASSWORD),
    ]
    cases = []
    for case_id, random_value, password in scenarios:
        authenticator = make_authenticator(
            encrypt_token=FIXTURE_ENCRYPT_TOKEN,
            user_id=FIXTURE_EPG_USER,
            stb_id=FIXTURE_STB_ID,
            ip_address=FIXTURE_IP_ADDRESS,
            mac=FIXTURE_MAC,
            password=password,
            custom_str=FIXTURE_CUSTOM_STR,
            random_value=random_value,
        )
        cases.append(
            Case(
                id=case_id,
                covers=("epg-auth",),
                input={
                    "random_value": random_value,
                    "encrypt_token": FIXTURE_ENCRYPT_TOKEN,
                    "user_id": FIXTURE_EPG_USER,
                    "stb_id": FIXTURE_STB_ID,
                    "ip_address": FIXTURE_IP_ADDRESS,
                    "mac": FIXTURE_MAC,
                    "custom_str": FIXTURE_CUSTOM_STR,
                    "password": password,
                },
                expected={"authenticator_hex": authenticator},
            )
        )
    return cases


def padding_cases() -> list[Case]:
    """PKCS#7 (authenticator) and zero (Option 60) padding vectors."""
    cases = []
    for length in (0, 8, 9):
        plaintext = bytes(range(length))
        cases.append(
            Case(
                id=f"padding-pkcs7-{length}",
                covers=(
                    "aligned-padding" if length % 8 == 0 else "non-aligned-padding",
                ),
                input={"kind": "pkcs7", "plaintext_hex": plaintext.hex()},
                expected={"padded_hex": pad(plaintext, DES.block_size).hex()},
            )
        )
    for length, label in ((0, "empty"), (8, "aligned"), (5, "non-aligned")):
        plaintext = bytes(range(length))
        padded = plaintext + b"\x00" * (DES.block_size - length % DES.block_size)
        aligned = length % DES.block_size == 0
        cases.append(
            Case(
                id=f"padding-option60-zero-{label}",
                covers=("aligned-padding",) if aligned else ("non-aligned-padding",),
                input={"kind": "option60-zero", "plaintext_hex": plaintext.hex()},
                expected={"padded_hex": padded.hex()},
            )
        )
    return cases
