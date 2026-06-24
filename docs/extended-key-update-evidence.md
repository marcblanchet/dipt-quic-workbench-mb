# Evidence: QUIC Extended Key Update in decrypted pcaps

This documents direct, decrypted-pcap evidence that the QUIC Extended Key Update extension
(`draft-ietf-quic-extended-key-update` / `draft-ietf-tls-extended-key-update`) actually updates
the connection keys.

## How the pcaps are produced

The `simulate` command writes one pcapng per node (`<node>.pcap`) and embeds the TLS secrets as a
pcapng *Decryption Secrets Block*, so Wireshark/tshark decrypt the QUIC payloads directly.

```sh
cargo run --release -p quinn-workbench -- simulate \
  --network-graph test-data/earth-mars/networkgraph-fullmars-extended-key-update.json \
  --network-events test-data/earth-mars/events.json \
  --traffic test-data/earth-mars/request-response.traffic.json

python3 scripts/eku-pcap-evidence.py GND.pcap ING.pcap   # GND = client, ING = server endpoint
```

The graph enables `extended_key_update` on all QUIC nodes and sets a 30-minute routine
`extended_key_update_interval_ms` on the client (`GND`, `192.168.40.1`). The server endpoint is
`192.168.43.2`. The Earth–Mars one-way delay is ~30 minutes.

## Evidence 1 — the `ExtendedKeyUpdate` exchange, decrypted byte-for-byte

The first extended key update happens under the initial 1-RTT secret, so tshark decrypts both
messages. The decrypted CRYPTO-frame handshake bytes from the **client** pcap (`GND.pcap`):

```
frame  8 (client → server): tls.handshake = 1a 000025 00 001d 0020 105ffacca2110b22…a50d1441
frame 13 (server → client): tls.handshake = 1a 000025 01 001d 0020 1e80c4f0679b8c1c…d1943b17
```

Decoding the request (frame 8):

| bytes        | value                | meaning                                            |
|--------------|----------------------|----------------------------------------------------|
| `1a`         | 26                   | HandshakeType = **ExtendedKeyUpdate** (provisional)|
| `00 00 25`   | 37                   | handshake message length                           |
| `00`         | 0                    | eku_type = **key_update_request**                  |
| `00 1d`      | 0x001d               | KeyShareEntry.group = **X25519**                   |
| `00 20`      | 32                   | key_exchange length                                |
| `105ffacc…1441` | 32-byte X25519 pub | fresh ephemeral public key                         |

Frame 13 is identical except `eku_type = 01` (**key_update_response**) and a *different* 32-byte
key share — i.e. a genuine fresh (EC)DHE exchange, not a key ratchet.

The **server** pcap (`ING.pcap`) shows the mirror image, and the 32-byte key shares match exactly
across the two captures:

```
                client pcap (GND)                                  server pcap (ING)
request  key:   105ffacca2110b2253c79d602354ad06552f884ea0d63382123ca23ea50d1441   (identical)
response key:   1e80c4f0679b8c1cd9b940371fe0f7bd964bbce628be92435dc6bebbd1943b17   (identical)
                request != response  →  fresh ephemeral keys on each side
```

So the actual on-the-wire key-exchange messages that establish the new secrets are present and
decrypted.

## Evidence 2 — the Key Phase bit flips

The header-protection key is not changed by a key update, so tshark decrypts the short headers and
the Key Phase bit is visible on **every** 1-RTT packet — including the later updates whose payloads
are not decryptable. Per sender, in capture order (`0`/`1` = Key Phase):

```
client 192.168.40.1:  000000 1111 00000 11111 00000 11    -> 5 key updates
server 192.168.43.2:  00000000 11111 00000 11111 0000     -> 4–5 key updates
```

Each toggle is one key update. On the client the first toggle (KP0→KP1) lands at t≈5407s — exactly
after it receives the `key_update_response` (frame 13, t≈5405s) and derives the new keys, as the
draft requires. The server's outgoing Key Phase then follows one round-trip later (it flips only
once it has received and decrypted a packet with the flipped bit). The toggles recur every ~2
one-way delays as the routine 30-minute timer drives successive updates over the multi-hour
connection, and the request/response transfer still completes (`DONE … request/response amount =
10`) — the connection stays in sync across every update.

## Note on later updates

Only the **first** exchange's messages decrypt: Wireshark derives QUIC key-update secrets via the
RFC 9001 `quic ku` ratchet, which does not apply to the extended key update's fresh-ECDHE secrets,
so it cannot decrypt traffic protected by the post-update secrets. The Key Phase bit (protected
only by the unchanged header key) remains visible throughout, which is why all of the update
toggles are observable even though their payloads are not.
