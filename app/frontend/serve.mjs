#!/usr/bin/env node
/**
 * MoNote 前端本地静态服务器（仅开发/演示用，Tauri 运行时不使用）。
 *
 * 用法：
 *   node frontend/serve.mjs            # http://127.0.0.1:8099
 *   node frontend/serve.mjs 8123       # 指定端口
 *
 * 作用：浏览器禁止 file:// 加载 ES Module，因此本地预览需经 HTTP 提供。
 * 未连接 Tauri 时前端自动进入 Mock 演示模式（见 js/mock.js）。
 */
import { createServer } from 'node:http';
import { readFile, stat } from 'node:fs/promises';
import { extname, join, normalize, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(fileURLToPath(new URL('.', import.meta.url)));
const PORT = Number(process.argv[2] || process.env.PORT || 8099);
const HOST = process.env.HOST || '127.0.0.1';

const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.md': 'text/markdown; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.jpg': 'image/jpeg',
  '.jpeg': 'image/jpeg',
  '.gif': 'image/gif',
  '.webp': 'image/webp',
  '.ico': 'image/x-icon',
  '.woff': 'font/woff',
  '.woff2': 'font/woff2',
  '.ttf': 'font/ttf',
  '.map': 'application/json; charset=utf-8',
  '.txt': 'text/plain; charset=utf-8',
};

const server = createServer(async (req, res) => {
  try {
    const url = new URL(req.url || '/', `http://${req.headers.host || HOST}`);
    let pathname = decodeURIComponent(url.pathname);
    if (pathname === '/') pathname = '/index.html';
    const target = normalize(join(ROOT, pathname));
    if (!target.startsWith(ROOT + sep) && target !== ROOT) {
      res.writeHead(403, { 'content-type': 'text/plain; charset=utf-8' });
      res.end('Forbidden');
      return;
    }
    const info = await stat(target);
    if (info.isDirectory()) {
      res.writeHead(302, { location: `${pathname.replace(/\/$/, '')}/index.html` });
      res.end();
      return;
    }
    const body = await readFile(target);
    const type = MIME[extname(target).toLowerCase()] || 'application/octet-stream';
    res.writeHead(200, {
      'content-type': type,
      'cache-control': 'no-store',
      'access-control-allow-origin': '*',
    });
    res.end(body);
  } catch (err) {
    const code = err && err.code === 'ENOENT' ? 404 : 500;
    res.writeHead(code, { 'content-type': 'text/plain; charset=utf-8' });
    res.end(code === 404 ? '404 Not Found' : `500 Internal Server Error\n${err}`);
  }
});

server.listen(PORT, HOST, () => {
  // eslint-disable-next-line no-console
  console.log(`MoNote 前端已启动： http://${HOST}:${PORT}/`);
  console.log('（Mock 演示模式；真实功能请使用 `cargo run` 启动 Tauri 应用）');
});

process.on('SIGINT', () => {
  server.close(() => process.exit(0));
});
process.on('SIGTERM', () => {
  server.close(() => process.exit(0));
});
