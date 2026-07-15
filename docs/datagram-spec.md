# Datagram Support — Architecture Specification
## Overview
Datagrams in libp2p (per [specs#680](https://github.com/libp2p/specs/pull/680)) are
MTU-sized, unreliable messages carried by native transport datagram APIs (QUIC
DATAGRAM frames). Each datagram flow is associated with a `/dg/1` control stream
that binds a QUIC stream ID to an application protocol.
The key architectural decision: **`/dg/1` is managed by the connection
infrastructure, not by individual protocol handlers.** This avoids
multistream-select collisions when multiple datagram-capable protocols share a
connection.
## 1. `/dg/1` Advertisement
The connection infrastructure advertises `/dg/1` as a supported protocol once
per connection. This is invisible to individual handlers — they do not add
`/dg/1` to their `listen_protocol()`.
## 2. Control Stream Lifecycle
When a `/dg/1` control stream is established (inbound or outbound), the
connection infrastructure:
- **Inbound**: reads `[uvarint(len)][app_proto_id]` from the stream, records
  `stream_id → app_proto_id`
- **Outbound**: writes `[uvarint(len)][app_proto_id]` to the stream, records
  `stream_id → app_proto_id`
- Keeps the stream open for the lifetime of the connection (per spec)
## 3. Outbound Datagram Flow
1. Handler calls `poll()` → returns
   `ConnectionHandlerEvent::SendDatagram(data: Bytes)`
2. Connection infrastructure looks up the handler's associated stream ID (from
   the control stream binding)
3. Connection infrastructure frames: `[QUIC varint(stream_id)][data]`
4. Connection infrastructure calls `muxing.send_datagram(framed)`
## 4. Inbound Datagram Flow
1. Muxer yields `StreamMuxerEvent::Datagram(raw)`
2. Connection infrastructure parses: `[QUIC varint(control_stream_id)][payload]`
   from `raw`
3. Connection infrastructure looks up `control_stream_id` → `app_proto_id` →
   handler
4. Connection infrastructure delivers `ConnectionEvent::Datagram { data: payload
   }` to that handler
Unknown `control_stream_id`s are silently dropped (per spec).
## 5. Handler Combinator Routing
For `ConnectionHandlerSelect` (and `Either`, `MultiHandler`):
- During `FullyNegotiatedInbound`/`Outbound` for a `/dg/1` stream: the
  combinator records `stream_id → child_handler`
- On `ConnectionEvent::Datagram`: the combinator routes by looking up
  `control_stream_id` in its child map, delivering to exactly one child
- Unknown stream IDs are dropped
This is the only change to handler combinators — they maintain a `HashMap<u64,
HandlerIndex>` populated from `/dg/1` stream negotiations.
## 6. Handler API
### ConnectionEvent
```rust
pub enum ConnectionEvent<'a, IP: InboundUpgradeSend, OP: OutboundUpgradeSend, IOI = (), OOI = ()> {
    // ...existing variants...
    Datagram(Datagram<'a>),
}
pub struct Datagram<'a> {
    pub data: &'a Bytes,
    // stream_id already stripped by the connection
}
```
### ConnectionHandlerEvent
```rust
pub enum ConnectionHandlerEvent<TUpgrade, TOutboundOpenInfo, TCustom> {
    // ...existing variants...
    SendDatagram(Bytes),
}
```
No `supports_datagrams()` on `ConnectionHandler`, no `stream_id` field on
`FullyNegotiatedInbound`/`Outbound` — the stream ID mapping is built at the
connection level before events reach handlers.
## 7. Handler Combinator Behavior
| Combinator | Datagram behavior |
|---|---|
| `ConnectionHandlerSelect` | Routes by internal `control_stream_id → child` map; maintains map from `/dg/1` `FullyNegotiated` events |
| `Either` | Same pattern as Select |
| `MultiHandler` | Routes by `control_stream_id → key` map |
| `MapOutEvent` | Pass-through to inner handler |
| `ToggleConnectionHandler` | Pass-through to inner handler if enabled |
| `OneShotHandler` | Ignores `Datagram`; never emits `SendDatagram` |
| `PendingConnectionHandler` | Ignores |
| `Dummy` | Ignores |
## 8. Trait Additions

```rust
/// Transport-assigned stream identifier, e.g. a QUIC stream id.
pub trait StreamId {
    fn id(&self) -> Option<u64>;
}

pub trait StreamMuxer {
    type Substream: AsyncRead + AsyncWrite + StreamId;
    fn send_datagram(&mut self, data: Bytes) -> Result<(), SendDatagramError>;
    fn max_datagram_size(&self) -> Option<usize> { None }
}

pub enum StreamMuxerEvent {
    AddressChange(Multiaddr),
    Datagram(Bytes),
}
```
## 9. Transport Layer (QUIC)
- `quinn` datagrams are enabled (remove `datagram_receive_buffer_size(None)`)
- `send_datagram` delegates to `quinn::Connection::send_datagram()`
- `StreamMuxerEvent::Datagram` surfaced from `quinn::Connection::read_datagram()`
- QUIC substream type implements `StreamId` returning the QUIC stream id
## 10. Things Explicitly Not Included
- No standalone `libp2p-datagram` crate (no `Behaviour`, `Handler`, or
  `Control`)
- No `supports_datagrams()` on `ConnectionHandler`
- No `stream_id` on `FullyNegotiatedInbound`/`Outbound` — mapping is built at
  the connection level before events reach handlers
