// Re-export of the build-time-generated third-party license disclosure. This
// thin wrapper gives unit tests a stable module to `vi.mock` instead of the
// `virtual:thirdparty-licenses` module (which would otherwise shell out to
// pnpm during the build).
export { default as THIRD_PARTY_LICENSES } from 'virtual:thirdparty-licenses'
