from __future__ import annotations

from pathlib import Path

import typer
from aiohttp import web

from .credentials import (
    DEFAULT_DHCP_PASSWORD,
    DEFAULT_DHCP_USER,
    DEFAULT_EPG_PASSWORD,
    DEFAULT_EPG_USER,
)
from .epg import EDS_AUTH_URL
from .logo_catalog import DEFAULT_CATALOG_PATH, DEFAULT_CONVERTER_URL
from .service import DEFAULT_GUIDE_CACHE_PATH, IptvService, ServiceConfig
from .web import create_app

app = typer.Typer(no_args_is_help=True)
_LOGO_CATALOG_OPTION = typer.Option(
    DEFAULT_CATALOG_PATH,
    help="Persisted local channel metadata and logo catalog",
)
_GUIDE_CACHE_OPTION = typer.Option(
    DEFAULT_GUIDE_CACHE_PATH,
    help="Persisted live XMLTV programme cache",
)


@app.command()
def serve(
    interface: str = typer.Option("eth0", help="IPTV interface"),
    epg_mac: str | None = typer.Option(
        None,
        envvar="IPTV_EPG_MAC",
        help="EPG authentication MAC; defaults to the interface MAC",
    ),
    stb_id: str = typer.Option("", help="EPG STBID; defaults to empty"),
    dhcp_user: str = typer.Option(
        DEFAULT_DHCP_USER, envvar="IPTV_DHCP_USER", help="DHCP username"
    ),
    dhcp_password: str = typer.Option(
        DEFAULT_DHCP_PASSWORD, envvar="IPTV_DHCP_PASSWORD", help="DHCP password"
    ),
    user_id: str = typer.Option(
        DEFAULT_EPG_USER, envvar="IPTV_USER_ID", help="EPG username"
    ),
    password: str = typer.Option(
        DEFAULT_EPG_PASSWORD, envvar="IPTV_PASSWORD", help="EPG password"
    ),
    authentication_url: str = typer.Option(EDS_AUTH_URL, envvar="IPTV_AUTH_URL"),
    host: str = typer.Option("127.0.0.1", help="HTTP bind address"),
    port: int = typer.Option(8080, min=1, max=65535, help="HTTP bind port"),
    guide_past_days: int = typer.Option(7, min=0, help="Past guide days"),
    guide_future_days: int = typer.Option(2, min=0, help="Future guide days"),
    guide_timezone: str = typer.Option("Asia/Shanghai", help="XMLTV timezone"),
    guide_cache: Path = _GUIDE_CACHE_OPTION,
    logo_catalog: Path = _LOGO_CATALOG_OPTION,
    metadata_converter_url: str = typer.Option(
        DEFAULT_CONVERTER_URL,
        help="51zmt metadata converter base URL",
    ),
) -> None:
    config = ServiceConfig(
        interface=interface,
        dhcp_user=dhcp_user,
        dhcp_password=dhcp_password,
        epg_user=user_id,
        epg_password=password,
        epg_mac=epg_mac,
        stb_id=stb_id,
        authentication_url=authentication_url,
        guide_past_days=guide_past_days,
        guide_future_days=guide_future_days,
        guide_timezone=guide_timezone,
        guide_cache_path=guide_cache,
        logo_catalog_path=logo_catalog,
        metadata_converter_url=metadata_converter_url,
    )
    web.run_app(create_app(IptvService(config)), host=host, port=port)


def main() -> None:
    app()
