# Wire schemas

`wire.schema.json` describes the current serial protocol as JSON Schema Draft
2020-12. `commands.json` maps every command to its request and response definitions,
streaming behavior and required capabilities. It also lists every notification.
The firmware does not advertise this command catalog at runtime.

Generate or check from the repository root:

```sh
(cd rust && cargo run -p cordial-schema)
(cd rust && cargo run -p cordial-schema -- --check)
(cd rust && cargo test -p cordial-schema)
```

The protocol crate's optional `schema` feature derives Schemars definitions from
its Serde types. `cordial-schema` builds the envelopes, command associations and
constraints imposed by custom decoding. Firmware does not enable this feature.

The root schema accepts a request, response or event. For useful response
validation, select `#/$defs/COMMAND.response` using the initiating request's
command. The response ID supplies correlation; the response itself has no command
name. Each definition distinguishes terminal and streamed success payloads from
terminal errors. `#/$defs/Request` and `#/$defs/Event` are union entry points.

Tests cover every command and notification, intermediate and terminal results,
invalid requests through both schema and the real codec, and incorrect response
phases. Firmware application tests validate emitted frames against these schemas.
The checked-in files must match reproducible generation.

JSON Schema does not check session ordering, IDs increasing across messages,
capability/readiness rules, heartbeat deadlines, cancellation, event revisions or
file mutation during a download. These remain in [the protocol documentation](../docs/protocol/README.md)
and the session implementation. JSON Schema sees parsed objects and cannot detect
duplicate JSON keys. Character lengths also cannot express UTF-8 byte lengths:
PIN reply values remain limited to 16 bytes by the codec, and the complete encoded
frame remains limited to 4096 bytes including LF. ASCII identifier/path limits
are represented exactly. Persisted filesystem documents are outside this schema.

Examples and test data are synthetic and contain no real bond keys.
