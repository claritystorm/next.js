---
name: next-dev-loop
description: >
  Verify Next.js runtime behavior after editing app code. Use this
  skill to confirm a change actually works in a running app — not
  just that it compiles or type-checks. Combines /_next/mcp
  (Next.js's view) with agent-browser (the browser's view).
  Requires a running `next dev`.
disable-model-invocation: true
metadata:
  draft: true
---

# next-dev-loop

The edit/verify rhythm during `next dev` — make a change, then
confirm it actually works at runtime, not just that the types or
the build are happy.

You verify through two views of the same running app:

- **`/_next/mcp`** — an HTTP endpoint Next.js exposes about itself.
  Knows framework-specific things: routes, segments, RSC, server
  actions, server logs, and errors as Next.js saw them (this
  includes browser-side runtime errors that bubbled up to the dev
  server). Call `tools/list` for the current surface.
- **`agent-browser`** — a CLI that drives a real Chrome. Knows
  framework-agnostic browser things: DOM, console, network, React
  fiber, vitals. Run `agent-browser --help` for the current surface.

The two views cross-check each other.

## requires

- Next.js **16.3+** with **Turbopack** — `/_next/mcp` plus the
  proactive compile check via `get_compilation_issues`.
- `agent-browser` **>= 0.27.0** — when React introspection landed.

These are hard floors, not soft preferences. If anything is missing,
tell the user how to upgrade and stop. Don't fall back to grepping
source or to a weaker probe — this skill assumes both views are live
at the versions above.

- Upgrade Next.js: `pnpm next upgrade` (or `npx next upgrade`).
  Docs: https://nextjs.org/docs/app/getting-started/upgrading
  (version-16 guide:
  https://nextjs.org/docs/app/guides/upgrading/version-16)
- Upgrade `agent-browser`: `npm i -g agent-browser@latest`.

## preflight

Once per session, confirm both views are live.

1. POST `tools/list` to `/_next/mcp`.
   - Unreachable → either `next dev` isn't running, or Next.js is
     below 16.3. Check `package.json` to disambiguate, then refuse.
   - `get_compilation_issues` not in the list → Next.js below 16.3.
     Refuse and tell the user to upgrade.
2. `mcp get_compilation_issues` doubles as a Turbopack probe.
   An error response of `"Turbopack project is not available..."`
   means the user is on webpack. Refuse — Turbopack is required.
3. `mcp get_routes` → your route map for the rest of the session.

## loop

After an edit, you want to know three things: does it compile, did
it actually land in the browser, and does the runtime match your
intent? Each maps to one or both views.

**Compilation**: `mcp get_compilation_issues`. Preflight already
guaranteed it's there.

**Landing in the browser**: if no `agent-browser` session is open,
open one against the user's installed Chrome with react-devtools
enabled. Then navigate to the route you actually touched. A hard
reload is the right call when verifying server-side changes (Server
Components, route handlers, middleware, layouts, server actions); a
soft client-router nav is the right call when verifying client-only
behavior. Don't verify on a stale tab.

Once a page has loaded, `mcp get_page_metadata` returns the files
that actually rendered it — segments, layouts, server actions,
client components. Use it as a discovery shortcut: load the route
you're about to edit, ask which files power it, and treat those as
entry points. This stays cheap as the codebase grows; agentic search
across `app/` doesn't. The same applies to `get_server_action_by_id`
for tracing a hashed action back to its source.

**Matching intent**: ask both views the same question. Where they
agree, the edit worked. Where they disagree, you're looking at a
real bug or a stale tab — reload and re-check.

## gotchas

- `react-devtools` is enabled at session open. Missed it → close
  and reopen.
- React introspection output is stale after navigation. Re-run.
- Non-3000 dev server: read the `next dev` banner; set
  `NEXT_MCP_URL=http://localhost:<port>/_next/mcp`.
- `get_errors` and `get_page_metadata` need at least one navigation
  to populate.

## reference

All tools below are present once preflight passes. If `tools/list`
is missing any of them, preflight should have refused — re-check.

```
# /_next/mcp                 notes
get_project_metadata         projectPath, devServerUrl, bundler
get_routes                   fs-scan; no browser session needed
get_errors                   runtime + build; needs a browser session;
                             includes browser-side errors caught by the
                             dev server
get_page_metadata            segment trie + routerType; needs a browser
                             session; use as a discovery shortcut for
                             which files power a route
get_logs                     returns logFilePath
get_server_action_by_id      hashed id → file + functionName
get_compilation_issues       Turbopack only; errors on webpack
                             ("Turbopack project is not available")
```

For `agent-browser`, run `agent-browser --help`. The surface is
expected to grow.

## teardown

Close the `agent-browser` session. Leave `next dev` up for the next
loop.

---

`next-dev-loop-<topic>` siblings (e.g. `next-dev-loop-rsc`, `next-dev-loop-debug`)
assume this preflight already ran; they pick up at the loop.
