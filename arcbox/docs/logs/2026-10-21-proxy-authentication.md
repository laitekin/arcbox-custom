# Host-side proxy authentication

Status: connector and TCP egress tests verified; settings UI and VM coverage in progress.

## Change

SOCKS5 credentials are decoded from explicit proxy URLs, validated against the
RFC 1929 byte limits, and carried by shared, redacted credential objects. Password
storage is zeroized on drop. URL Display omits the password, and parser errors do
not include the original URL.

Both TCP CONNECT and UDP ASSOCIATE use the same greeting and authentication.
Configured credentials require method 0x02; a server choosing no-auth is rejected.
HTTP CONNECT supports Basic authentication and maps 407 to a typed authentication
error without echoing the server's status text.

## Evidence and reproduction

```sh
cargo test -p arcbox-fakeip
cargo test -p arcbox-proxy --test authenticated --test http_auth
cargo test -p splicetcp --test proxy_rejection
cargo clippy -p arcbox-fakeip -p arcbox-proxy -p splicetcp --all-targets -- -D warnings
```

The controlled SOCKS5 fixture verifies the authenticated TCP and UDP wire paths,
data relay, rejected passwords, and refusal of no-auth downgrade. The TCP egress
regression also listens at the real destination and confirms no direct connection
is made after proxy rejection. These are host-level tests, not evidence that
every VM or daemon download already shares the policy; that is tracked in #2.
