---
name: Design proposal
about: Suggest a schema change, new sub-command surface, or architectural shift
title: 'design: '
labels: kind:design
---

## Motivation

<!-- What problem does this solve? Why now? -->

## Proposed change

<!-- Config schema, CLI surface, struct shapes, code sketches. -->

```toml
# proposed .krypt.toml surface
```

## Alternatives

<!-- Sub-bullet alternatives + why rejected. -->

## Migration cost

<!-- Breaking change? Impact on existing .krypt.toml configs and deploy behaviour. -->

## Stability impact

- Breaks `.krypt.toml` schema (existing configs stop parsing)? [ ]
- Breaks `krypt deploy` behaviour? [ ]
- Changes `krypt-core` public API? [ ]
- Adds new required toml keys? [ ]
- Affects manifest / drift-detection format (version bump needed)? [ ]
