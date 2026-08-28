# S6 Spike Report — Version-pinned reconstruction (upcast fixture)

Status: complete. Date: 2026-08-28. Disposable spike code: `spikes/s6-upcast/` (never promoted to the implementation).

## Question

Does the R-06 contract hold in practice: kernel-validated envelopes, verbatim custom payloads, typed interpretation only via known pure upcasters, opaque-but-inspectable unknowns, and precise partial availability for missing objects — with no module code on the reconstruction path? This is the M1 deliverable shape (version field + versioned-record registry + one exercised fixture).

## Method

- Fixture stream (S3 frame format): four events — `user_message` v1, `tool_result` v1 (x2, one referencing an existing object, one referencing a missing object), `future_kind` schema 9 (unknown).
- Versioned-record registry: `(kind, payload_schema) -> pure Rust upcaster`; unknown entries return None → opaque.
- Reconstruction: chain-verified stream read + per-event envelope validation + registry upcast + object-reference existence check against the object store.

## Results

```
events=4
  user_message: schema=1 count=1 upcasted=1 opaque=0
  tool_result:  schema=1 count=2 upcasted=2 opaque=0
  future_kind:  schema=9 count=1 upcasted=0 opaque=1 reason="no upcaster for kind 'future_kind' schema 9"
  missing_objects=["blake3:deadbeef"]
  upcast_errors=[]
```

Upcast throughput (100k events): 1.3M ev/s — registry lookup + pure transform is not a reconstruction bottleneck.

## Findings

1. **The contract holds as specified**: typed interpretation is a projection layered only where the upcaster is known; unknown kinds reconstruct as opaque-but-inspectable with a precise reason; missing required objects are reported exactly, never fabricated; upcasters are pure kernel Rust (no module code on the path).
2. **Partial availability is precise and per-kind**: the report distinguishes upcasted / opaque / missing-ref at event granularity — the M1 acceptance "reconstruct or report precise partial availability" is satisfied by this shape.
3. Upcast cost is negligible (1.3M ev/s), so the versioned-record registry can run per event at rebuild scale (S5: 1M ev/s) without special casing.

## M1 inputs

- The versioned-record registry + one exercised fixture (this exact shape) is confirmed as an M1 deliverable; the generic upcaster machinery stays deferred (R-20) as specified.
- Reconstruction report shape (per-kind schema/count/upcasted/opaque + missing objects + upcast errors) is a candidate for the M1 audit-reconstruction output contract.
