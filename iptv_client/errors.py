from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True, slots=True)
class IptvError(Exception):
    operation: str
    detail: str

    def __str__(self) -> str:
        return f"{self.operation}: {self.detail}"
