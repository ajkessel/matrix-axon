import js from '@eslint/js'
import tseslint from 'typescript-eslint'
import reactHooks from 'eslint-plugin-react-hooks'
import prettier from 'eslint-config-prettier'
import globals from 'globals'

export default tseslint.config(
  { ignores: ['dist/', 'src/api/schema.d.ts'] },
  js.configs.recommended,
  tseslint.configs.recommended,
  {
    files: ['**/*.{ts,tsx}'],
    languageOptions: {
      globals: globals.browser,
    },
    plugins: {
      'react-hooks': reactHooks,
    },
    rules: {
      ...reactHooks.configs.recommended.rules,
      // Assumes React state semantics: it forbids writing to anything a hook
      // returned. This app's state is @preact/signals, whose *designed*
      // mutation idiom is `signal.value = x` — including signals reached
      // through the useServices() context hook.
      'react-hooks/immutability': 'off',
    },
  },
  {
    // The e2e lane and its config/mock run in Node, not the browser.
    files: ['e2e/**', 'playwright.config.ts'],
    languageOptions: {
      globals: globals.node,
    },
  },
  prettier,
)
