import nextra from 'nextra'

const withNextra = nextra()

export default withNextra({
  reactStrictMode: true,
  output: 'export',
  trailingSlash: true,
  basePath: process.env.PAGES_BASE_PATH || ''
})
