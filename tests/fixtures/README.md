These fixed ACMP frames were generated with Linkr's Go protobuf message types,
deterministic protobuf marshaling and Go's standard CRC32C implementation.

The peer session ID is sixteen `0x02` bytes; the device session ID is sixteen
`0x01` bytes. The text is `Linkr → Linkrd 文本同步\nsecond line` with a newline.
The device first reports unavailable text at revision 1. The write then publishes
revision 2, message 3, followed by its successful response in message 4. Both
refer to peer message 2.

The fixtures check that the Linkrd Rust implementation preserves the wire
contract in both directions, including request correlation and write confirmation.
