# Alex Runtime Nextra 文档

此目录是面向应用开发者和使用者的 Nextra 4 文档站。仓库根目录的 `docs/` 继续保存架构、状态和设计资料。

## 本地运行

```powershell
Set-Location docs-site
npm install
npm run dev
```

浏览器打开 `http://localhost:3000`。

## 构建静态站点

```powershell
npm run build
```

静态文件生成到 `docs-site/out/`。GitHub Pages 构建时通过 `PAGES_BASE_PATH` 设置项目站点路径。

