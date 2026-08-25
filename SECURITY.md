# Security Policy

## Supported Versions

The library is in active development; only the latest version is supported.

## Reporting a Vulnerability

Please report **any** security issue you find by
[opening a new issue](https://github.com/ziv/godotiles/issues/new) and marking it as a security
issue. We will respond as soon as possible and work with you to resolve it.

## Notes

Tile downloads use HTTPS with certificate verification (rustls). There is no option to disable
verification. Tiles are written to the on-disk cache under the configured `cache_dir`; treat that
directory as untrusted input from the network (it is only ever decoded as images).
