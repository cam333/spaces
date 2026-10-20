# Security policy

Spaces runs terminals and coding agents as you. A flaw in it can hand
someone a shell on your machine, so security reports get priority.

## Reporting a vulnerability

Please report privately, not in a public issue:

1. Go to <https://github.com/jlongo78/spaces/security>.
2. Choose **Report a vulnerability**.
3. Describe the problem, the version (`npm ls -g @jlongo78/agent-spaces`),
   your operating system, and the steps or proof of concept that show it.

You should get an acknowledgement within a few days. Once a fix is ready we
agree on a disclosure date with you, publish a GitHub security advisory, and
credit you unless you'd rather not be named.

## Supported versions

| Version | Supported |
| --- | --- |
| 1.x | Yes |
| Earlier releases | No |

Fixes go into the latest 1.x release. Upgrade with `spaces upgrade`.

## Scope

In scope: the `@jlongo78/agent-spaces` package and this repository, which is
the server, the web app, the terminal client, the `spaces` and `spaces-msg`
commands and the service installer, running in **desktop mode** (the
default).

Especially interesting:

- anything that lets another account on the same machine, a web page in the
  owner's browser, or anything on the network reach the server as the owner;
- a way to read or write files outside the file browser's confinement
  (below);
- credentials that leak into URLs, logs, process command lines or files that
  other accounts can read;
- `spaces --remote` exposing anything beyond what the user's SSH login
  already allows.

Out of scope:

- The experimental multiuser and federation modes. Their known weaknesses are
  listed below; reports about them are welcome as ordinary issues.
- What an agent does once it is running. Spaces runs agents with your
  permissions on purpose (see below).
- Vulnerabilities in the agents themselves, or in Node.js, the browser or
  the operating system, unless Spaces makes them reachable when they
  otherwise would not be.
- Attacks that need the owner's account already (read access to
  `~/.spaces/internal_token` is the owner's account, as far as Spaces is
  concerned).

## Threat model

### What desktop mode guarantees

Desktop mode assumes a machine that other people may also have accounts on,
and a browser that visits untrusted sites.

- **Only the owner.** The server answers only requests that prove they come
  from the account that started it. The proof derives from a 32-byte secret
  in `~/.spaces/internal_token`, readable only by that account (mode 0600 in
  a 0700 directory; on Windows, inside the user's profile). The server fixes
  loosened permissions at start-up and refuses to start if the file belongs
  to someone else. Arriving over the loopback interface proves nothing.
- **Loopback only.** The public port and the internal web-app port both
  listen on `127.0.0.1`.
- **No rebinding.** Every request must name the loopback (or a host the
  owner configured in `SPACES_ALLOWED_ORIGINS`) in its `Host` header, so a
  web page that rebinds its own domain to `127.0.0.1` is refused.
- **Browser sessions.** A browser signs in only through a single-use link
  that expires after two minutes (`spaces web`) and gets an HttpOnly,
  SameSite=Strict cookie that is accepted only on requests from Spaces' own
  pages. Sessions last seven days; `spaces signout` replaces the secret and
  ends every session at once.
- **No credentials in URLs.** Tokens travel in headers and cookies only.
  The secret itself never goes on the wire: clients get tokens signed with
  it. On Linux, the sign-in link is not passed on the browser's command line,
  where other accounts could read it.
- **No squatting.** On Linux, before handing a credential to a server, the
  launcher checks that the process holding the port belongs to the same
  account.
- **Remote use stays on SSH.** `spaces --remote` reaches the other machine
  only through the user's own SSH connection. The remote server stays on its
  loopback, and the remote's secret never leaves it.
- **File browser confinement.** The file browsers in the web app and the
  terminal client reach only the configured dev directories (or the home
  folder when none are set) and the agents' transcript folders, after
  resolving symlinks. Credential folders such as `~/.spaces`, `~/.ssh`,
  `~/.gnupg`, `~/.aws`, `~/.azure`, `~/.kube`, `~/.docker`,
  `~/.config/gcloud` and `~/.config/gh`, and files like `~/.netrc`,
  `~/.npmrc` and `~/.git-credentials`, are always refused.
- **Pane environments.** Panes get an allowlisted environment; the server's
  own secrets are not passed to the programs they run.

### What it does not guarantee

- **Anything running as you is you.** An agent in a pane, a shell, or any
  program you run can read `~/.spaces/internal_token` and do everything you
  can, with or without Spaces. Spaces does not sandbox agents. Treat a prompt
  injected into an agent as code running under your account.
- **Experimental multiuser mode** (`--multiuser`) is not safe. It listens on
  every interface, and **any signed-in user can currently gain control of
  the account the server runs as.** Don't enable it on a machine that
  people you don't fully trust can reach.
- **Experimental federation** (`--federate`) is not safe. **A paired machine
  can act as you, and traffic between machines is not encrypted.** Use
  `spaces --remote` to work on another machine.
- **Exposing the port yourself.** Putting the port behind a reverse proxy,
  a tunnel or a port forward that other people can reach is outside the
  model, even with authentication in place.
- **Outbound requests.** Spaces checks npm and GitHub for updates and
  downloads agent-detection rules from `agentspaces.co` (see the README).
  Detection rules are data: they are size-capped, validated, fetched over
  HTTPS only, and never executed, but they are not signed.
