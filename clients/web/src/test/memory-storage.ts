/**
 * Minimal in-memory `Storage` for tests. Stores take their storage as a
 * parameter precisely so tests don't depend on the environment's
 * `localStorage` (which jsdom under Node 25 exposes as a bare object).
 */
export function memoryStorage(initial: Record<string, string> = {}): Storage {
  const items = new Map(Object.entries(initial))
  return {
    get length() {
      return items.size
    },
    key: (index: number) => [...items.keys()][index] ?? null,
    getItem: (key: string) => items.get(key) ?? null,
    setItem: (key: string, value: string) => void items.set(key, value),
    removeItem: (key: string) => void items.delete(key),
    clear: () => items.clear(),
  }
}
