# xmip-core-transport-j1939

J1939 transport: SAE J1939 over CAN — 29-bit identifiers of priority, parameter group and source address, and the transport protocol, BAM and RTS/CTS, for a payload of up to 1785 bytes. A technology of [xmip-core-transport](https://github.com/IlleNilsson/xmip-core-transport).

A send target is read by `net::Target` in [xmip-core-library-net](https://github.com/IlleNilsson/xmip-core-library-net), the one reading of a URI every technology calls, and its query is decoded there. Until 2026-09-28 this technology split the query off itself, without percent-decoding it.

A `0x` number in a target is read by `codec::hex::prefixed_number` in [xmip-core-library-codec](https://github.com/IlleNilsson/xmip-core-library-codec), which refuses a sign; until 2026-09-28 it was read with `from_str_radix`, which took `0x+7e8`.

## Acknowledgement

A transfer by RTS/CTS is acknowledged after the whole receive cycle: the
sender waits for the end of message acknowledgement, sent on Accepted; a
connection abort with reason 255 (any other reason, J1939-21 section 5.10.3.5),
sent on Refused, which fails the sender's send as permanent so it does not send
the group again (J1939-21 has no abort reason that says refused, and 255 is the
one that does not ask for a repeat); or a connection abort with reason 2
(resources needed elsewhere), sent on Failed, which fails the sender's send as
retryable so it sends the group again. A
single frame and a broadcast announced by BAM are answered by nobody:
acceptance is at-most-once there. Each parameter group arrives whole.

## Toolchain

`rust-toolchain.toml` pins the toolchain for the whole estate. Do not change it
here.

## Verification

The included workflow is manual-only and calls the versioned shared workflow at
`IlleNilsson/.github@v1`.
