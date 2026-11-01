from __future__ import annotations

import secrets
import time
from hashlib import md5

from Crypto.Cipher import DES

from .credentials import DEFAULT_DHCP_PASSWORD, DEFAULT_DHCP_USER

_PREFIX = b"\x00\x00\x1f"
_ENCRYPTION_METHOD = 1
_R_LENGTH = 8
_TS_LENGTH = 8
_HASH_KEY_LENGTH = 16


def _crypt(key: bytes, data: bytes, *, decrypt: bool) -> bytes:
    first = DES.new(key[0:8], DES.MODE_ECB)
    second = DES.new(key[8:16], DES.MODE_ECB)
    third = DES.new(key[16:24], DES.MODE_ECB)
    if decrypt:
        return first.decrypt(second.encrypt(third.decrypt(data)))
    return third.encrypt(second.decrypt(first.encrypt(data)))


def _session_key(r: bytes, ts: bytes) -> bytes:
    if len(r) != _R_LENGTH or len(ts) != _TS_LENGTH:
        raise ValueError("R and TS must each be exactly 8 bytes")
    return r + ts + b"\x00" * 8


def encode_option60(*, r: bytes, ts: bytes, hash_key: bytes, login: str) -> bytes:
    if len(hash_key) != _HASH_KEY_LENGTH:
        raise ValueError("HashKey must be exactly 16 bytes")
    plaintext = login.encode("utf-8")
    padded = plaintext + b"\x00" * (DES.block_size - len(plaintext) % DES.block_size)
    ciphertext = _crypt(_session_key(r, ts), padded, decrypt=False)
    declared_length = 1 + len(r) + len(ts) + len(hash_key) + len(ciphertext)
    if declared_length > 0xFF:
        raise ValueError("CTC Option 60 exceeds the one-byte length field")
    value = _PREFIX + bytes([declared_length])
    value += bytes([_ENCRYPTION_METHOD]) + r + ts + hash_key + ciphertext
    return value


def make_option60(
    password: str = DEFAULT_DHCP_PASSWORD,
    login: str = DEFAULT_DHCP_USER,
    *,
    r: bytes | None = None,
    ts: bytes | None = None,
) -> bytes:
    """Generate the exact container used by the recovered local dhcpcd."""
    if r is None:
        r = secrets.randbits(31).to_bytes(_R_LENGTH, "little")
    if ts is None:
        ts = (time.monotonic_ns() // 1_000_000).to_bytes(_TS_LENGTH, "little")
    hash_key = md5(r + password.encode() + ts).digest()
    return encode_option60(r=r, ts=ts, hash_key=hash_key, login=login)
