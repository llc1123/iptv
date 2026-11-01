from __future__ import annotations

from hashlib import md5
from secrets import randbelow

from Crypto.Cipher import DES
from Crypto.Util.Padding import pad


def identity_encode(session_id: str, share_key: str) -> str:
    return md5(f"{session_id}{share_key}".encode()).hexdigest().upper()


def make_authenticator(
    *,
    encrypt_token: str,
    user_id: str,
    stb_id: str,
    ip_address: str,
    mac: str,
    password: str,
    custom_str: str = "$CTC",
    random_value: int | None = None,
) -> str:
    random_text = str(randbelow(100_000_000) if random_value is None else random_value)
    plaintext = f"{random_text}${encrypt_token}${user_id}${stb_id}${ip_address}${mac}${custom_str}"
    key = password.encode()[:24].ljust(24, b"0")
    first = DES.new(key[0:8], DES.MODE_ECB)
    second = DES.new(key[8:16], DES.MODE_ECB)
    third = DES.new(key[16:24], DES.MODE_ECB)
    padded = pad(plaintext.encode(), DES.block_size)
    encrypted = third.encrypt(second.decrypt(first.encrypt(padded)))
    return encrypted.hex().upper()
