# Network capability ownership

`I2pEndpoint` accepts only `.i2p` names, base32 I2P names, or destination-like encoded values. It is not a URL or socket address. Upstream bytes can be acquired only through `I2pStreamProvider`. `LocalAcceptor` is a separate downstream contract. No SAM implementation, host resolver, TCP connector, HTTP client, or router administration API is in this workspace.

The routine boundary script scans core/wire manifests and sources and checks a forbidden-network positive control.
