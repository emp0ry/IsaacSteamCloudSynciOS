# IsaacSteamSynciOS vendoring note

This is `steam-cm-protocol` 0.4.1 under its MIT license, sourced from the
published crates.io package.

IsaacSteamSynciOS uses QR/device and credential authentication, CM
transport, protobuf messages, the server directory, and refresh-token parsing.
The upstream credential facade is retained, but its unconditional `rsa`
dependency is removed. The credential path performs only the public
RSAES-PKCS1-v1_5 operation Valve requires, using `num-bigint` and OS-seeded
randomness.

This narrow change avoids linking an unused RSA implementation affected by
RUSTSEC-2023-0071. All retained protocol behavior remains upstream 0.4.1 code.
