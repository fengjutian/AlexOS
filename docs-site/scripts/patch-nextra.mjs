import { readFile, writeFile } from 'node:fs/promises'
import { fileURLToPath } from 'node:url'

const layoutUrl = new URL('../node_modules/nextra-theme-docs/dist/layout.js', import.meta.url)
const layoutPath = fileURLToPath(layoutUrl)
const broken = 'LayoutPropsSchema.safeParse(themeConfig)'
const fixed = 'LayoutPropsSchema.safeParse({ children, ...themeConfig })'

const source = await readFile(layoutPath, 'utf8')

if (source.includes(fixed)) {
  console.log('Nextra Layout patch already applied')
} else if (source.includes(broken)) {
  await writeFile(layoutPath, source.replace(broken, fixed))
  console.log('Applied Nextra Layout children validation patch')
} else {
  throw new Error(
    'Unsupported nextra-theme-docs layout.js; remove the patch or update it for the installed version'
  )
}
