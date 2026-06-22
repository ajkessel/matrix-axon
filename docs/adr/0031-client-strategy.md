# ADR 0031 — Multi-platform client strategy

## Context

`axon-tui` proved the `/v1/` API surface during the MVP. The post-MVP roadmap
(tech-spec §Roadmap signposts) calls for additional clients targeting web
browsers, iOS, Android, and desktop (macOS, Windows, Linux). No per-client
plan existed; this ADR records the approach before implementation begins.

Several constraints shape the decision:

- **Single API surface.** Clients consume `/v1/` HTTP REST + the WebSocket at
  `/v1/ws`, not the Matrix homeserver directly. The server is the deliverable;
  clients are consumers of a stable contract.
- **Bearer token auth today; OAuth 2.0 + PKCE later.** The wire protocol
  (`Authorization: Bearer`) won't change (ADR 0029), but the mint flow will.
  Clients must not hard-wire the token-paste bootstrap path. OAuth 2.0+SSO 
  with common providers (Google, Apple, Microsoft) is a near-term implementation 
  goal.
- **Browser WebSocket auth.** Browsers cannot set `Authorization` on a
  WebSocket upgrade, so the server accepts `Sec-WebSocket-Protocol:
  bearer.<token>` from browser clients (ADR 0029). Web and Tauri clients must
  use this path.
- **Push notifications are P0 post-MVP.** APNs (iOS) → FCM (Android) → web
  push. Mobile clients must be designed to register push tokens from day one,
  even if the server-side push router isn't wired yet.
- **OpenAPI spec is the contract.** The spec is checked into the repo and is
  the source of truth for every `/v1/` operation. Generated SDK stubs for Swift
  already ship as part of the MVP build; other platforms should follow the same
  pattern.
- **Media proxy contract.** The in-flight media proxy work (`matrix-api-media-proxy`
  branch) fixes the media URL shape. Clients must not assume direct homeserver
  media URLs; all media is served through the axon `/v1/` surface.

## Decision

**Approach: native per platform.** Each client uses the idiomatic toolkit for
its target rather than a shared cross-platform framework. This gives
best-in-class UX, full access to platform push and credential APIs, and no
lowest-common-denominator abstractions. The trade-off is three separate
codebases rather than one; the OpenAPI-generated stubs are the mechanism that
keeps them consistent with the server contract without hand-rolling HTTP calls.

**iOS client: SwiftUI, targeting iOS 17+.** The OpenAPI-generated Swift stubs
shipped as part of the MVP form the networking layer. APNs push-token
registration is a day-one concern in the client architecture even before the
server-side push router exists. Directory: `clients/apple/` (see macOS entry
below).

**macOS (desktop): SwiftUI multiplatform, sharing the iOS Swift Package.** The
iOS project is structured as a Swift Package with a shared `axon-core` library
(networking, models, business logic) and platform-specific UI targets. The
macOS target reuses `axon-core` with a native macOS SwiftUI UI — not
Mac Catalyst. Both live under `clients/apple/` as targets within the same
package.

**Android client: Kotlin + Jetpack Compose, targeting Android 10 (API 29)+.**
FCM push-token registration is a day-one concern in the client architecture,
mirroring the iOS stance above. Directory: `clients/android/`.

**Windows / Linux desktop: Tauri, delivered alongside the web client.** A
Tauri shell (`src-tauri/` config directory) wraps the web SPA in a native
desktop app using the OS's own WebView — Edge WebView2 on Windows 10+,
WebKitGTK on Linux. This produces a ~5–10 MB installer with no bundled
Chromium. The server is already Rust, so Tauri uses the same toolchain
(`cargo tauri build`). Tauri support lives inside `clients/web/` alongside the
SPA; no separate directory is needed. Target: ship Windows and Linux desktop
builds as soon as the web SPA stabilizes, at near-zero marginal cost.

**Web client: TypeScript SPA with Vite — framework to be decided.** The SPA
consumes `/v1/` over `fetch` and the native `WebSocket` API (using the
`Sec-WebSocket-Protocol: bearer.<token>` path). It is hosted separately from
the server; no SSR is required. The web client is the design reference: its
component library and screen flows inform the mobile clients. Directory:
`clients/web/`.

The JavaScript framework is an open question that must be resolved before the
web client milestone begins:

|                            | React                            | Vue 3                       | Svelte                    | Preact                              |
| -------------------------- | -------------------------------- | --------------------------- | ------------------------- | ----------------------------------- |
| **Ecosystem / components** | Largest (shadcn/ui, Radix, etc.) | Large                       | Smaller                   | React-compatible (via compat layer) |
| **TypeScript**             | Excellent                        | Excellent (Composition API) | Good, less mature         | Excellent (mirrors React)           |
| **OpenAPI code-gen**       | Most mature tooling              | Good                        | Less mature               | Same as React tooling               |
| **Tauri integration**      | Best-documented                  | Good                        | Less-documented           | Good (same as React)                |
| **Boilerplate**            | Moderate                         | Low–moderate                | Very low                  | Moderate                            |
| **Bundle size**            | Moderate                         | Moderate                    | Very small (compile-time) | Very small (~3 KB)                  |
| **Developer availability** | Highest                          | High                        | Lower                     | High (React devs transfer easily)   |

React is the lowest-risk default; Vue 3 is a legitimate alternative with a
cleaner API; Svelte is compelling for performance and simplicity but carries
ecosystem and tooling risk; Preact offers near-identical React API with a
fraction of the bundle size via its compatibility layer.
**Team should discuss and decide before the web client milestone is started.**

**Sequencing: Web (+Tauri desktop) → iOS → Android → macOS.** Web ships
first: no app-store approval, fastest feedback loop, validates design patterns
the other clients follow. Tauri Windows/Linux desktop ships alongside it at
near-zero marginal cost. iOS ships second, because APNs is the P0 push target
and the Swift stubs already exist. Android third, because FCM follows APNs.
macOS last, because it depends on the iOS Swift Package being stable.

## Consequences

- Three client codebases: `clients/web/` (SPA + Tauri shell), `clients/apple/`
  (shared Swift Package with iOS and macOS targets), `clients/android/`.
- **For further discussion**: at what point do we break the clients into separate
  repos/workspaces. Pros and cons of monorepo versus clean separation.
- The OpenAPI spec becomes a first-class contract artifact. Breaking changes to
  `/v1/` require coordinated updates across all generated SDKs.
- OAuth 2.0 + PKCE (post-MVP) will replace the bearer-token paste flow for web
  and mobile clients. Bearer-token paste is acceptable alpha onboarding only
  for `axon-tui`; mobile and web clients should implement proper login UX once
  OAuth lands.
- Push notification support requires server-side additions (APNs/FCM
  integration, device-token registration endpoint) before mobile clients can
  deliver notifications. Client code should stub the registration path and
  activate it when the server ships the feature.
- Media URLs are axon-proxied; clients must not construct homeserver media URLs
  directly. The `matrix-api-media-proxy` branch establishes this contract.
- The web-framework choice is the one unsettled decision. It must be resolved —
  and recorded as a follow-on ADR or amendment here — before `clients/web/`
  work begins.
