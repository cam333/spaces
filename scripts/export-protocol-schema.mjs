import { createRequire } from 'node:module';
import { writeFileSync, readFileSync, mkdirSync, existsSync } from 'node:fs';
import path from 'node:path';

const require = createRequire(import.meta.url);
const { z } = require('zod');
const { zodToJsonSchema } = require('zod-to-json-schema');
const { PROTOCOL_VERSION, schemas } = require('../bin/lib/runtime/protocol.js');

// None of these Zod schemas use .strict() — the server tolerates and
// silently strips unknown request keys, including future additive v1
// protocol fields. zod-to-json-schema's default for a plain (non-strict)
// z.object is still `additionalProperties: false`, which would make
// third-party validators (and a future generated Rust client) hard-reject
// payloads the server actually accepts. `removeAdditionalStrategy: 'strict'`
// makes it emit the server's real, lenient contract instead.
//
// `$refStrategy: 'root'` alone is not enough to dedupe shared subschemas
// here: each entry used to come from its own independent `zodToJsonSchema`
// call, and within a single call a subschema only gets hoisted into a `$ref`
// when *that call* encounters it more than once. `PaneInfo` appears exactly
// once inside `Snapshot`'s own tree — the fact that `PaneInfo` is *also*
// exported as its own top-level schema was invisible to Snapshot's call, so
// it was still fully inlined as an anonymous, unnamed duplicate (which a
// downstream Rust codegen step then has to invent a synthetic name for,
// e.g. `SnapshotPanesItem`, distinct from the real `PaneInfo` type).
//
// zod-to-json-schema's `definitions` option seeds a call's "already seen"
// map with named schemas up front (by Zod object identity), so any of them
// encountered while walking a tree resolve to a `$ref` instead of being
// inlined. But feeding each entry only its *siblings* (excluding itself) is
// unsound: `getRefs()` seeds `seen` with the *correct* `#/definitions/<name>`
// path for every schema passed in `definitions` before any of them are
// walked — so a schema that's excluded from its own call (because it *is*
// that call's subject) has no such seeded entry, and if another sibling's
// forced resolution reaches it first (e.g. resolving `PaneInfo` walks into
// `agentState: AgentState`), it gets registered under whatever incidental
// nested path it was first found at (`#/definitions/PaneInfo/properties/
// agentState`) instead of its own. That produces a corrupted, misdirected
// `$ref` for that schema's *own* top-level entry — not just an inlined
// duplicate, an actively wrong document.
//
// The reliable fix is a single shared call: resolve all twelve schemas
// together via `definitions` (self-inclusive is fine — forced resolution
// always recomputes the real body regardless of what's pre-seeded) against
// an unrelated placeholder `main`, so every cross-reference resolves
// consistently regardless of iteration order. Each of the twelve named
// entries below then republishes the *same* resolved definitions map under
// its own `$ref`, keeping every entry a self-contained draft-07 document.
const resolved = zodToJsonSchema(z.unknown(), {
  $refStrategy: 'root',
  removeAdditionalStrategy: 'strict',
  definitions: schemas,
});

const out = {
  $schema: 'http://json-schema.org/draft-07/schema#',
  protocol: PROTOCOL_VERSION,
  schema_version: 1,
  schemas: Object.fromEntries(
    Object.keys(schemas).map((name) => [
      name,
      {
        $ref: `#/definitions/${name}`,
        definitions: resolved.definitions,
        $schema: 'http://json-schema.org/draft-07/schema#',
      },
    ]),
  ),
};

const target = path.resolve('docs/protocol/spaces-proto.schema.json');
const json = JSON.stringify(out, null, 2) + '\n';

if (process.argv.includes('--check')) {
  const current = existsSync(target) ? readFileSync(target, 'utf-8') : '';
  if (current !== json) {
    console.error('protocol schema is stale — run: npm run proto:schema');
    process.exit(1);
  }
  console.log('protocol schema is current');
} else {
  mkdirSync(path.dirname(target), { recursive: true });
  writeFileSync(target, json);
  console.log(`wrote ${target}`);
}
