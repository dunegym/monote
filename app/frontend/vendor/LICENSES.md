# MoNote 前端第三方库（vendor）清单

本目录内所有库均为**离线本地文件**，运行期不依赖任何 CDN / 网络请求（NFR-108）。
所有文件通过 `npm pack` 从 npm registry 下载官方发布包后原样提取。

| 库 | 版本 | 本地文件 | 用途 | 许可证 |
| --- | --- | --- | --- | --- |
| [marked](https://www.npmjs.com/package/marked) | 18.1.2 | `marked.umd.js`（`lib/marked.umd.js`） | Markdown → HTML（CommonMark + GFM） | MIT（见 `marked.LICENSE`） |
| [KaTeX](https://www.npmjs.com/package/katex) | 0.19.1 | `katex/katex.min.js`、`katex/katex.min.css`、`katex/fonts/*` | 行内/块级数学公式渲染 | MIT（见 `katex/LICENSE`） |
| [highlight.js](https://www.npmjs.com/package/@highlightjs/cdn-assets) | 11.12.0 | `highlight/highlight.min.js`（common languages 构建） | 围栏代码块语法高亮 | BSD-3-Clause（见 `highlight/LICENSE`） |

下载方式（可复现）：

```powershell
npm pack marked@18.1.2 katex@0.19.1 "@highlightjs/cdn-assets@11.12.0"
# 解包后分别提取 lib/marked.umd.js、dist/katex.min.* + dist/fonts/、highlight.min.js
```

说明：

- `marked.umd.js` 暴露 `window.marked`，`katex.min.js` 暴露 `window.katex`，`highlight.min.js` 暴露 `window.hljs`；
  `index.html` 在 ES Module 入口之前以普通 `<script>` 同步加载它们，页面初次加载不发起任何网络请求。
- KaTeX 字体目录保留 `.ttf/.woff/.woff2` 三种格式以兼容各平台 WebView。
- 使用方（`js/preview.js`）对第三方库缺失做降级：库不存在时预览退化为纯文本/代码不高亮，不抛异常。
