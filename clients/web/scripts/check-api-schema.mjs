import { execFileSync } from 'node:child_process'
import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const scriptDir = dirname(fileURLToPath(import.meta.url))
const webRoot = resolve(scriptDir, '..')
const repoRoot = resolve(webRoot, '..', '..')
const schemaPath = join(webRoot, 'src', 'api', 'schema.d.ts')
const tempDir = mkdtempSync(join(tmpdir(), 'axon-web-schema-'))
const tempSchemaPath = join(tempDir, 'schema.d.ts')
const pnpm = process.platform === 'win32' ? 'pnpm.cmd' : 'pnpm'

try {
  try {
    execFileSync(
      pnpm,
      [
        'exec',
        'openapi-typescript',
        join(repoRoot, 'openapi', 'openapi.json'),
        '-o',
        tempSchemaPath,
      ],
      { cwd: webRoot, stdio: 'inherit' },
    )
  } catch (error) {
    console.error(
      [
        'Failed to generate clients/web/src/api/schema.d.ts for comparison.',
        'Run `pnpm install --frozen-lockfile` in clients/web, then retry `pnpm --dir clients/web check:api`.',
      ].join('\n'),
    )
    process.exitCode =
      typeof error === 'object' &&
      error !== null &&
      'status' in error &&
      typeof error.status === 'number'
        ? error.status
        : 1
  }

  if (process.exitCode === undefined) {
    const committed = readFileSync(schemaPath, 'utf8')
    const generated = readFileSync(tempSchemaPath, 'utf8')

    if (committed !== generated) {
      console.error(
        [
          'clients/web/src/api/schema.d.ts is out of date.',
          'Run `pnpm --dir clients/web gen:api` and commit the result.',
        ].join('\n'),
      )
      process.exitCode = 1
    }
  }
} finally {
  rmSync(tempDir, { recursive: true, force: true })
}
