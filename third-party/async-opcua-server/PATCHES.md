## NG-Gateway Local Patches

This directory vendors `async-opcua-server 0.18.0` so NG-Gateway can ship
deployment-aware endpoint fixes before they are available from crates.io.

### Preserve externally advertised endpoint ports in `run_with`

Upstream `Server::run_with(listener)` overwrote the configured endpoint port
with `listener.local_addr().port()` before logging and serving endpoint
descriptions. That is correct for tests using port `0`, but wrong for
production deployments where the server listens on an internal socket and is
published through Docker, Kubernetes NodePort, or a reverse proxy.

For NG-Gateway the `advertised_endpoints` configuration is the operator-owned
source of truth for client-facing URLs. The local patch keeps the configured
`host()` / `port()` values when `run_with(listener)` is used, so
`GetEndpointsResponse.EndpointDescription.endpointUrl` contains the externally
reachable URL instead of the container-internal bind port.

Validation:

- `cargo test -p async-opcua-server`
- `cargo test -p ng-plugin-opcua-server`

When upgrading `async-opcua`, remove this local patch only after confirming
that `run_with(listener)` no longer rewrites configured public endpoint ports
or provides an explicit API for public endpoint URLs.
