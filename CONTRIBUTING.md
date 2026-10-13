# Contributing

Thanks for helping. Bug reports, fixes and features are all welcome. For
anything larger than a small fix, please open an issue first so we can agree
on the approach before you spend time on it.

Security problems go through private reporting, not issues: see
[SECURITY.md](SECURITY.md).

## Setup

You need Node.js 20.9+ and, for the terminal client, Rust (the version is
pinned in `rust-toolchain.toml`; `rustup` picks it up automatically). On
Linux, `node-pty` compiles during install and needs `python3`, `make` and a
C++ compiler.

```bash
git clone https://github.com/jlongo78/spaces.git
cd agent-spaces
npm ci
npm run build        # production build of the web app
npm run dev          # or: the dev server, with hot reload
```

```bash
cd tui
cargo build --release -p spaces-tui
```

In a checkout, `node bin/spaces.js tui` uses the binary you just built.
On Windows, close any running terminal client before rebuilding: the build
cannot replace an executable that is in use.

Spaces keeps its state in `~/.spaces` and serves port 3457. If you already
use Spaces day to day, test your changes on another port
(`node bin/spaces.js --port 3460`), and prefer a throwaway `HOME` for
anything that writes to `~/.spaces`.

## Before you open a pull request

Run what CI runs.

**Node / TypeScript**

```bash
npm test                 # the whole vitest suite (npx vitest run)
npm run lint
npm run proto:check      # the protocol schema matches the runtime
npm run proto:docs       # every protocol method is documented
```

If you change the protocol (`bin/lib/runtime/protocol.js` and friends), run
`npm run proto:schema` to regenerate `docs/protocol/spaces-proto.schema.json`
and document the change in `docs/protocol/README.md`.

**Rust** (from `tui/`)

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test -p spaces-tui --test live_runtime -- --ignored   # needs npm ci first
```

New behaviour comes with tests. Bug fixes come with a test that fails
without the fix.

## Commits

Use [Conventional Commits](https://www.conventionalcommits.org/), with a
scope where it helps:

```
feat(tui): drag files onto the Files tab to upload them
fix(cli): say why the background server failed instead of waiting it out
docs(protocol): document the methods the reference was missing
```

The subject line describes the change from the user's point of view: what
now works, or what was wrong. The body, when there is one, explains why.
Keep each commit to one change.

## Agent-detection rules

The blocked/working/done rules for each agent live in
`bin/lib/runtime/detect/manifests/`. They ship with every release, and
installs also fetch newer ones daily from agentspaces.co, so a rule fix need
not wait for a release. After changing a manifest, bump its `version`, then
publish it to the site's repository:

```bash
node scripts/publish-detection.js ../spaces_web
```

and commit and push the site.

## License

By contributing you agree that your contributions are licensed under the
[Apache License 2.0](LICENSE), the same as the project.
