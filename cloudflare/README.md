# Meowtify jam relay on Cloudflare Workers

This Worker introduces a jam's host and guests to each other. It carries small JSON messages
only: WebRTC signaling and who is in the room. Songs and playback state travel straight from the
host to each guest, never through the Worker.

It runs the same room logic as the self-hosted relay (`crates/meowtify-jam-relay`). One Durable
Object holds one room. Rooms use WebSocket Hibernation, so an idle room costs nothing.

## Deploy

You need a Cloudflare account, Node.js and the Rust toolchain with the `wasm32-unknown-unknown`
target.

```
rustup target add wasm32-unknown-unknown
cd cloudflare
npx wrangler deploy
```

`wrangler deploy` builds with `worker-build` (see `[build]` in `wrangler.toml`) and publishes
the Worker. Its address, such as `https://meowtify-jam.<you>.workers.dev`, becomes the relay
address in the app: `wss://meowtify-jam.<you>.workers.dev`.

In Meowtify, open the Jam page, paste that address into the relay field, press Enter and press
Start a jam. Guests need nothing but the link.

## TURN (optional, recommended)

Most pairs connect directly through STUN. Some home networks block that, and those guests need
a TURN server to relay their audio. Cloudflare Realtime offers one with a free allowance.

1. In the Cloudflare dashboard open Realtime, then TURN, and create a TURN key.
2. Store its two values as secrets:

```
npx wrangler secret put TURN_KEY_ID
npx wrangler secret put TURN_API_TOKEN
```

Without the secrets the Worker's `/turn` answers with STUN only. A TURN relay carries audio for
the guests that need it, so it uses your TURN allowance.

## Limits and abuse

- A room holds the host and up to 8 guests. Each member may send a short burst of frames and 20
  per second after that. Frames over 16 KB are refused.
- Anyone who knows the Worker's address can create a room. Room codes are 10 random letters and
  digits, so a code cannot be guessed, but nothing stops a stranger from hosting on your Worker.
  The Worker is cheap to run, and an idle room costs nothing. To restrict it anyway, put
  Cloudflare Access or a WAF rate limit in front of it.
- The Worker never sees songs. Its logs hold room codes, so keep logging off unless you debug.

## Develop

```
cd relay/cloudflare
cargo check --target wasm32-unknown-unknown
npx wrangler dev
```

This directory is its own Cargo workspace, outside the main one, because it only builds for
`wasm32`.
