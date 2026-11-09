# Behaviour worth knowing

Details an operator or a maintainer needs, kept out of the README so it stays
usable as a quick start.

## DHCP renewal

At half the lease time (T1) the server sends a renewal REQUEST carrying the
address it holds in `ciaddr`, a freshly computed Option 60 authenticator, and
the granting server as destination. It does not send options 50 or 54, because
those belong to a full acquisition.

This matters: measured on the live network, a DISCOVER at T1 was answered with a
**different** address, while the renewing REQUEST kept the original binding. A
captured set-top box renews the same way, except that it also unicasts at layer
two — which would need the server's MAC, so our frame stays broadcast there.

A refused renewal (`NAK`) falls back to a full DISCOVER/REQUEST exchange.
Timeouts and I/O errors are retried until expiry instead, because the held lease
is still valid and a DISCOVER could move the address. If the lease expires, or
the provider assigns a different address, the process exits with status 1 so the
supervisor restarts it on the new lease.

## Routing isolation

The leased address is added without a prefix route. Its subnet and a default
route via the lease's router go into one routing table, reachable only by
traffic from the leased address (`ip rule` at the same priority, ahead of
`main`). Both numbers come from `--route-table` (default 4788, "IPTV" on a
keypad; valid range 1–32765 so the rule stays ahead of `main`).

The EPG and HLS clients bind to that address and the interface, so they reach
the provider whatever the host's own routes or overlapping subnets are, and the
host's other traffic never enters the IPTV network. No provider subnets are
assumed or hardcoded.

On `down`, the address comes off and the rule and table are cleared, so repeated
starts do not accumulate rules.

## One subscription, one session

The provider binds the EPG account to the set-top-box MAC, one session each.
Measured 2026-11-09: two instances on distinct DHCP MACs but a shared
`--epg-mac` both got leases and both authenticated, but only the **most recent**
login could fetch a live playlist. The displaced one returned

```
503 epg-tvod: EPG response could not be parsed: hls address: expected value at line 35 column 1
```

and did not recover until restarted — while its cached M3U and XMLTV kept
answering 200, because the guide lives in memory. That combination is easy to
misread as a network fault. Run one instance per subscription; concurrency needs
a second subscription with its own MAC.

## Guide refresh and provider throttling

The provider answers a large share of concurrent programme-window requests with
HTTP 503. The server retries the failed windows with lower concurrency and
backoff. Windows that still fail keep the previous snapshot's programmes, so a
refresh never wipes the guide; a partly failed refresh is never treated as
success.

## Logo directory

The provider's channel data carries no logos (`ChannelLogURL` is always empty),
so logos are the operator's to supply: a channel's logo is
`<logo-dir>/<channel-id>.png` (`--logo-dir`). Channels with such a file get a
`tvg-logo`/`icon` pointing at `/logos/<channel-id>`; the rest get none. Files are
checked on each feed render, so a logo added later appears after the next
refresh.
