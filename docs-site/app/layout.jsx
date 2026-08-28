import { Footer, Layout, Navbar } from 'nextra-theme-docs'
import { Head } from 'nextra/components'
import { getPageMap } from 'nextra/page-map'
import 'nextra-theme-docs/style.css'
import './styles.css'

export const metadata = {
  title: { default: 'Alex Runtime 文档', template: '%s · Alex Runtime' },
  description: '创建、调试、打包和管理 Alex Runtime 桌面应用。'
}

const navbar = (
  <Navbar
    logo={<span className="alex-logo"><b>Alex</b><small>Runtime Docs</small></span>}
    projectLink="https://github.com"
  />
)

const footer = <Footer>Alex Runtime Developer Preview · MIT License</Footer>

export default async function RootLayout({ children }) {
  return (
    <html lang="zh-CN" dir="ltr" suppressHydrationWarning>
      <Head faviconGlyph="A" />
      <body>
        <Layout
          navbar={navbar}
          pageMap={await getPageMap()}
          docsRepositoryBase="https://github.com/AlexOS/AlexOS/tree/main/docs-site"
          footer={footer}
          sidebar={{ autoCollapse: true, defaultMenuCollapseLevel: 1 }}
          navigation={{ prev: true, next: true }}
        >
          {children}
        </Layout>
      </body>
    </html>
  )
}
