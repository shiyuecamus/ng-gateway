## NG-Gateway Local Patches

This directory vendors `async-opcua-core 0.18.0` so NG-Gateway can ship a
protocol-level compatibility fix before it is available from crates.io.

### `SecurityMode=None` OpenSecureChannel nonce

OPC UA Part 6 states that when `SecurityMode=None`, OpenSecureChannel messages
are not signed or encrypted, nonces are ignored, and nonce fields should be set
to null. The upstream `SecureChannel::create_random_nonce()` generated a
32-byte nonce even when `SecurityPolicy=None`, which made strict clients such as
KepServerEX close the connection immediately after the OpenSecureChannel
response. Prosys OPC UA Browser tolerates the non-null nonce, which is why the
issue only reproduced with KepServerEX at the customer site.

The local patch keeps nonces empty whenever either the selected
`SecurityPolicy` or `MessageSecurityMode` is `None`. Secure endpoints such as
`Basic256Sha256 + SignAndEncrypt` still generate policy-sized nonces and keep
the original key-derivation behavior.

Validation:

- `cargo test -p async-opcua-core`
- `cargo test -p ng-plugin-opcua-server`

When upgrading `async-opcua`, remove this local patch only after confirming that
the upstream `OpenSecureChannelResponse.server_nonce` is null for
`SecurityMode=None` and policy-sized for secure modes.
