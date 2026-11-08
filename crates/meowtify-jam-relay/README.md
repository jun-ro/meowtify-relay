# meowtify-jam-relay

A small server that introduces a jam's host and guests to each other. It is the self-hosted
alternative to the Cloudflare Worker in `relay/cloudflare`, and runs the same room logic.

It carries small JSON messages only (WebRTC signaling and who is in the room). Songs and
playback state go straight from the host to each guest.

## Run

```
cargo run --release -p meowtify-jam-relay
```

That is all. On start the relay finds its own public address, starts a TURN server next to
itself, and prints what to paste into Meowtify:

```
TURN is on at 203.0.113.5, UDP port 3478. Players can hide their IP address.
jam relay listening on 0.0.0.0:8787
Relay address for Meowtify: ws://203.0.113.5:8787
```

Options: an address to listen on (default `0.0.0.0:8787`), `--public-ip IP` (or the
`MEOWTIFY_PUBLIC_IP` variable) when the address cannot be found, and `--no-turn`.

A server with no firewall needs nothing else. If a firewall is on, allow TCP 8787, UDP 3478 and
UDP 49152-49351. The relay cannot open those itself.

## Hiding players' IP addresses

By default Meowtify connects through the relay's TURN server, so a host and its guests never
see each other's addresses. Audio then passes through this machine (about 20 KB/s per guest). A
player can choose "Share" on the Jam page to connect directly instead.

The operator of the relay can still see the addresses of the people who connect to it. Turn off
the access log of any proxy in front of it. The relay itself writes no addresses to its output.

The TURN credentials are made up on the fly from a key that is created at start and kept in
memory only. They expire after a day.

## `wss://`

The relay speaks plain `ws://`, which is fine for a private network or a quick start. For
`wss://`, put a TLS proxy in front and send `/jam/` and `/turn` to the relay. TURN stays on its
UDP ports, so use the machine's own address for it, not a proxied name.

Caddy:

```
jam.example.com {
    reverse_proxy 127.0.0.1:8787
}
```

Caddy gets the certificate and passes WebSockets through. Then the relay address is
`wss://jam.example.com`. The relay still announces TURN at the public address it found.

## Limits

A room holds the host and 8 guests. Each member gets a burst of 40 frames and 20 per second
after that. Anyone who can reach the relay can create a room, so run it on a private network or
behind a firewall if that matters to you.
