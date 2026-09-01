# Axon desktop shell

The native shell around the `clients/web` bundle (ADR 0102, M-W12). Desktop
today; iOS and Android are M-W13.

## Build it through the Tauri CLI, not cargo

```sh
cd clients/web
pnpm install
pnpm tauri dev     # dev loop, hot reload
pnpm tauri build   # release binary + installers
```

**`cargo build` / `cargo run` in this directory produce a binary that launches
and shows nothing but an error.** That is not a broken checkout. The frontend is
embedded at compile time from `../dist`, which is generated and gitignored, so
a fresh clone has none — and `tauri::generate_context!()` says nothing about it.
The CLI is what runs the frontend build first (`beforeBuildCommand`); cargo on
its own has no idea it needs to. The binary explains this if you hit it.

## macOS: build for both architectures

`tauri build` targets the host, so a build on Apple Silicon produces an arm64
bundle that **will not open on an Intel Mac**. (An x86_64 bundle does run on
Apple Silicon, under Rosetta 2 — the failure is one-directional, which makes it
easy to miss when testing on the newer machine.)

```sh
rustup target add aarch64-apple-darwin x86_64-apple-darwin
pnpm tauri build --target universal-apple-darwin
```

Both slices are lipo'd into one bundle. The Intel slice cross-compiles from
Apple Silicon; no second machine is needed, and the webview is a system
framework (WKWebView), so there is no per-architecture native library to
supply.

Note this is the _opposite_ of what `.github/workflows/cross-build.yml` does
for the server and TUI, which ship per-arch zips on purpose. ADR 0102 § 9 has
the reasoning for the divergence.

## Its own cargo workspace

Not a member of the repo root's. `cross-build.yml` runs `cargo build
--workspace` on three platforms and the pre-push gate runs `cargo clippy
--all-targets`; membership would have put webkit2gtk and a desktop app build on
every one of them. The root `Cargo.toml` names this directory in `exclude` so
cargo treats it as deliberate rather than an unlisted member.

The pre-push gate covers it separately, as `shell-fmt`, `shell-clippy` and
`shell-test`, filtered to this directory.

## Linux build dependencies

```sh
sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
```

Only needed if you actually touch this crate; nothing else in the repo links
against them.

## The dev server port is pinned

`beforeDevCommand` is `pnpm dev --strictPort`, so it fails if 5173 is taken
rather than drifting to 5174. `devUrl` in `tauri.conf.json` names 5173, and
Vite's default of quietly picking another port means the shell would otherwise
load whatever _else_ is on 5173 — a different app, with no error anywhere.

## Linux desktop entry

`bundle/axon.desktop` overrides Tauri's built-in template, and exists for one
character: the `%u` on `Exec`.

`%u` is the freedesktop field code that passes a URL to the program. Without
it the launcher starts the app with _no argument_, so an
`org.matrixaxon.axon:/oauth/callback?...` link resolves to this entry,
launches the app, and
the URL is silently dropped — the sign-in completes in the browser and the app
never hears about it. Tauri's default template has no field code, because
nothing tells it this app handles a URL scheme; the `deep-link` plugin
contributes the `MimeType` line but not the argument. The two halves have to
agree or the association is decoration.

`Name` is fixed rather than `{{name}}`. That variable is `productName`, which
is now `Axon` and would give the same answer — but it also names the deb
package and the macOS bundle, so pinning the launcher's `Name` keeps a future
packaging rename out of what the user reads in their menu.

## Icons

`icons/` is generated from `icon-source.png`, which is the supplied artwork
converted to PNG (2048x2048):

```sh
pnpm exec tauri icon src-tauri/icon-source.png -o src-tauri/icons
```

Two notes on the source. It arrived as a JPEG, so it carries no alpha channel
— which does not matter for this artwork, because the design is a _filled
tile_ rather than a glyph meant to float on transparency, and iOS masks its own
rounded corners from a filled square anyway. And JPEG is lossy, so the flat
background has faint compression noise baked into it (adjacent corner pixels
differ by a point or two where a flat field should be uniform). It is
imperceptible at every size an icon is displayed at, and the PNG here stops any
further loss, but a PNG or vector master would be cleaner if one exists.

## The bundle identifier is provisional

`org.matrixaxon.axon`. It becomes a permanent store identity and cannot be
changed later without shipping a new app, so settle it before any submission.
