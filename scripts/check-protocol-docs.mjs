import { readFileSync } from 'node:fs';
import path from 'node:path';

const catalogue = JSON.parse(
  readFileSync(path.resolve('tests/proto/fixtures/method-catalogue.json'), 'utf-8'),
);
const doc = readFileSync(path.resolve('docs/protocol/README.md'), 'utf-8');

const missing = catalogue.sync.filter(m => !doc.includes(m));
if (missing.length) {
  console.error(`docs/protocol/README.md does not document: ${missing.join(', ')}`);
  process.exit(1);
}
console.log(`protocol docs cover all ${catalogue.sync.length} methods`);
