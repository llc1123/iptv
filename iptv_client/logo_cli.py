from __future__ import annotations

from pathlib import Path

import anyio
import typer

from .credentials import (
    DEFAULT_DHCP_PASSWORD,
    DEFAULT_DHCP_USER,
    DEFAULT_EPG_PASSWORD,
    DEFAULT_EPG_USER,
)
from .epg import EDS_AUTH_URL
from .logo_catalog import (
    DEFAULT_CATALOG_PATH,
    DEFAULT_CONVERTER_URL,
    LogoCatalogBuilder,
)
from .service import IptvService, ServiceConfig

app = typer.Typer(no_args_is_help=True)
_OUTPUT_OPTION = typer.Option(
    DEFAULT_CATALOG_PATH,
    help="Catalog JSON path; raw and matched M3U files are stored beside it",
)


@app.command()
def build(
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
    output: Path = _OUTPUT_OPTION,
    converter_url: str = typer.Option(
        DEFAULT_CONVERTER_URL,
        help="51zmt metadata converter base URL",
    ),
    m3u_base_url: str = typer.Option(
        "http://127.0.0.1:8080",
        help="Base URL embedded in the raw M3U submitted for matching",
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
        logo_catalog_path=output,
        metadata_converter_url=converter_url,
    )

    async def run() -> None:
        service = IptvService(config)
        await service.start()
        try:
            catalog = await LogoCatalogBuilder(
                converter_url=converter_url,
            ).build(service.channels, output, m3u_base_url=m3u_base_url)
        finally:
            await service.close()
        typer.echo(
            f"wrote {output} ({len(catalog.entries)} channels, "
            f"{sum(bool(entry.logo_path) for entry in catalog.entries.values())} logos)"
        )

    anyio.run(run)


def main() -> None:
    app()
