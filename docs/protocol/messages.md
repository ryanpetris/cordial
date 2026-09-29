# Message envelopes

[Protocol index](README.md)

The examples below are wire messages. Each object occupies one line; formatted multi-line objects elsewhere in the protocol documentation describe data shapes only.

## Protocol discovery

On connect, query the protocol before sending versioned management commands:

```json
{"v":0,"id":1,"cmd":"adapter.protocol","args":{}}
{"v":0,"type":"response","id":1,"ok":true,"done":true,"result":{"protocol":1}}
```

The discovery request and successful response always use `v:0`. Their envelope
format and version remain fixed across firmware releases. `result.protocol` reports the
USB control protocol version, currently `1`. Clients ignore additional fields in
this result and stop the connection if they do not support the reported protocol.

Discovery accepts `args` as any JSON object, `null`, or an omitted field. Object
contents are ignored. Other argument types are invalid envelopes. The usual
message size limits and request ID rules apply, and discovery consumes its ID.
Discovery is available before Bluetooth and storage are ready. A valid discovery
request always succeeds. Unsupported versions and malformed envelopes use
`protocol.error` events in the adapter's current control protocol, outside the
fixed discovery exchange.

## Management messages

Request:

```json
{"v":1,"id":1,"cmd":"adapter.status","args":{}}
```

`id` is an integer from 1 through 2,147,483,647. IDs must increase strictly within a control session, including commands that return errors. They are correlation identifiers, not device identifiers or retry tokens. `cmd` is a case-sensitive command name and `args` is an object. Unknown commands and unknown arguments return errors.

Successful terminal response:

```json
{"v":1,"type":"response","id":1,"ok":true,"done":true,"result":{"protocol":1}}
```

Failed terminal response:

```json
{"v":1,"type":"response","id":2,"ok":false,"done":true,"error":{"code":"not_found"}}
```

The successful result above illustrates the envelope; the `adapter.status` command returns the full fields defined in [Status](commands.md#status). A response contains exactly one of `result` or `error`. Successful results are objects except for `adapter.capabilities`, whose result is an enum array. Errors contain a stable `code` and optional object `details`. The host supplies human-readable explanations; no display message is stored or sent by firmware. Device `last_error` uses the same code-only shape without details.

Every accepted request receives exactly one terminal response while its control session remains healthy. `device.list` also emits nonterminal responses with `"done":false`, each containing one device record. The `adapter.wait_ready` command can emit one nonterminal initialization response before its terminal response. HID++ list/refresh/apply and storage list/read also stream nonterminal results. The generated command catalog identifies each streaming command.

Asynchronous event:

```json
{"v":1,"type":"event","event":"device.unpaired","data":{"revision":18,"device_id":"d_7"}}
```

An event contains `event` and object `data`, and may contain `request_id` when associated with a request. It never uses the response field `id`. Authentication prompts and discovery results always carry `request_id`. They do not complete the request.

Responses to different requests may arrive out of order. A client continues reading events and dispatching responses by ID while waiting for a particular operation. Finishing a scan or pairing operation is reported by its terminal response; there is no second completion event to wait for.

Malformed envelopes, invalid/reused IDs, oversized lines, and unsupported versions produce an unsolicited `protocol.error` event with `data.code`. If safely recoverable, include the supplied ID as `data.supplied_id`, but do not treat it as a response or execute the request. A valid envelope with a bad command or arguments receives an ordinary terminal error response and consumes its ID. Reusing an ID must not affect the original request.
