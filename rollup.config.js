import { copyFileSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { cwd } from 'node:process'

const pkg = JSON.parse(readFileSync(join(cwd(), 'package.json'), 'utf8'))
const { import: esm, require: cjs } = pkg.exports

// `tsc` (TypeScript 7) emits the ESM build and type declarations into
// dist-js; rollup only derives the CommonJS build from that output, and the
// declarations for it: under `"type": "module"` TypeScript reads `index.d.ts`
// as describing an ES module, which a `require` cannot load (TS1479).
export default {
  input: esm.default,
  output: {
    file: cjs.default,
    format: 'cjs'
  },
  external: [
    /^@tauri-apps\/api/,
    ...Object.keys(pkg.dependencies || {}),
    ...Object.keys(pkg.peerDependencies || {})
  ],
  plugins: [
    {
      name: 'cjs-declarations',
      writeBundle() {
        copyFileSync(esm.types, cjs.types)
      }
    }
  ]
}
