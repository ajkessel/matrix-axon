/// <reference types="vite/client" />

declare const __AXON_WEB_VERSION__: string
declare const __AXON_WEB_BUILT_AT__: string

// Generated at build time by the `axon-thirdparty-licenses` plugin in
// vite.config.ts from the pnpm production dependency tree.
declare module 'virtual:thirdparty-licenses' {
  import type { ThirdPartyLicense } from './thirdparty-disclosure'
  const licenses: ThirdPartyLicense[]
  export default licenses
}
