<h1 align="center">Meowtify Relay</h1>

<p align="center">
  The server for <a href="https://github.com/jun-ro/meowtify">Meowtify</a> jam sessions.<br>
  One small binary. No config files, no port forwarding wizard, no TURN setup.
</p>

<p align="center">
  <a href="https://github.com/jun-ro/meowtify-relay/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/jun-ro/meowtify-relay/actions/workflows/ci.yml/badge.svg"></a>
  <img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-orange">
  <img alt="Rust 2021" src="https://img.shields.io/badge/rust-2021-orange">
  <img alt="Memory under 64 MB" src="https://img.shields.io/badge/memory-%3C%2064%20MB-brightgreen">
</p>

---

## What it does

A jam is a listening session where one person hosts and friends join by link. The relay is the
meeting point. It does two jobs:

1. **Signaling.** It introduces the host and the guests to each other and tracks who is in the
   room. It only forwards small JSON messages.
2. **TURN.** It runs a built-in TURN server, so guests can connect through the relay and never
   see each other's IP addresses.

Songs and playback state never touch the signaling part. When people choose to connect
directly, audio goes straight from the host to each guest. When IP hiding is on (the default in
Meowtify), audio passes through the TURN server on this machine instead, about 20 KB/s per guest.

## Quick start

Download the latest binary from the [releases page](https://github.com/jun-ro/meowtify-relay/releases)
and run it:

```
./meowtify-jam-relay
```

That is the whole setup. On start the relay finds its own public address, starts TURN next to
itself, and prints what to paste into Meowtify:

```
TURN is on at 203.0.113.5, UDP port 3478. Players can hide their IP address.
jam relay listening on 0.0.0.0:8787
Relay address for Meowtify: ws://203.0.113.5:8787
```

In Meowtify, open the Jam page, paste that address into the relay field and press Start a jam.
Guests only need the invite link.

A fresh VPS has no firewall rules, so this works as it is. If you run a firewall, allow:

| Port | Protocol | Used for |
|---|---|---|
| 8787 | TCP | Signaling (WebSocket) |
| 3478 | UDP | TURN |
| 49152 to 49351 | UDP | TURN relayed traffic |

The relay cannot open ports on your router. On a home network, forward those ports or host on
a VPS.

### Options

```
meowtify-jam-relay [ADDRESS:PORT] [--public-ip IP] [--no-turn]
```

| Option | Meaning |
|---|---|
| `ADDRESS:PORT` | Where to listen. Default `0.0.0.0:8787`. |
| `--public-ip IP` | The public address, when it cannot be found by itself. Also read from `MEOWTIFY_PUBLIC_IP`. |
| `--no-turn` | Run signaling only. Players then cannot hide their IP address. |

## Run it as a service

On a Linux machine with systemd:

```
sudo ./deploy/install.sh ./meowtify-jam-relay
```

This creates an unprivileged `meowtify-jam-relay` user, installs the binary to
`/usr/local/bin`, and starts a hardened systemd service that is limited to 64 MB of memory.
Read the address it prints with `journalctl -u meowtify-jam-relay`.

## Secure WebSockets (`wss://`)

The relay speaks plain `ws://`, which is fine for a quick start. For `wss://`, put a TLS proxy
in front and send `/jam/` and `/turn` to the relay. TURN keeps its own UDP ports, so it stays
on the machine's address and is not proxied.

Run the relay on loopback so only the proxy can reach it:

```
meowtify-jam-relay 127.0.0.1:8787
```

Caddy:

```
jam.example.com {
    reverse_proxy 127.0.0.1:8787
}
```

nginx: see [`deploy/nginx-jam.conf`](deploy/nginx-jam.conf). Then the relay address is
`wss://jam.example.com`.

If the proxy is behind a CDN such as Cloudflare, WebSockets work, but TURN does not (it is UDP),
so the relay still announces TURN at the machine's own address.

## Cloudflare Worker

Prefer serverless? The same room logic runs as a Cloudflare Worker with one Durable Object per
room. It uses Cloudflare's own TURN service. See [`cloudflare/README.md`](cloudflare/README.md).

## Privacy

Be clear about what this protects.

- Other people in the jam **cannot** see your IP address when hiding is on. They only see the
  TURN server's address.
- You, the person running the relay, **can** see the IP address of everyone who connects, and
  all audio passes through your machine. The relay program writes no addresses to its output,
  but your proxy or hosting provider might log them. Turn off access logs for `/jam/` and
  `/turn` if you want to keep none.
- TURN credentials are made up on the fly from a key that exists only in memory. They expire
  after a day. Nothing is stored on disk.

## Limits

- A room holds the host and 8 guests, and the relay keeps up to 500 rooms.
- Each member can send a burst of 40 frames, then 20 per second. Frames over 16 KB are refused.
- Anyone who can reach the relay can create a room. Run it on a private network or behind a
  firewall if that matters to you.

## How it is built

| Path | What it is |
|---|---|
| `crates/meowtify-jam` | The wire format and the room logic. No I/O and no clock, so it builds for native and wasm32. The Meowtify client uses it too. |
| `crates/meowtify-jam-relay` | The native server: WebSocket signaling, the built-in TURN server, and `GET /turn`. |
| `cloudflare/` | The same room logic as a Cloudflare Worker. |
| `deploy/` | A systemd unit, an installer and an nginx example. |

## Build from source

You need a recent stable [Rust toolchain](https://rustup.rs) and a C compiler (`build-essential`
on Debian and Ubuntu).

```
git clone https://github.com/jun-ro/meowtify-relay
cd meowtify-relay
cargo build --release -p meowtify-jam-relay
./target/release/meowtify-jam-relay
```

Run the tests:

```
cargo test --workspace
```

To check a relay that is already running, point the ignored end to end test at it. It gets
TURN credentials and passes a packet between two clients through the server:

```
MEOWTIFY_RELAY_HTTP=https://jam.example.com cargo test -p meowtify-jam-relay --test remote -- --ignored
```

## License

[MIT](LICENSE)
