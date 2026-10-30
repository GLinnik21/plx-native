// Render a Design-canvas artifact's artboards to PNG so a reviewer (or the design-board
// workflow's agents, through Read) can see them rather than read their HTML.
//
// usage: node .claude/design-board/render-canvas.js <artifact-files-dir> <out-dir> [Board.dc.html ...]
//   <artifact-files-dir> is the folder the Artifact tool's `read` saved the canvas into: it must
//   hold project/*.dc.html, project/ds/** and artifact-type/dc-runtime.js (read those paths first).
//   Uploaded assets (`/_blob/<id>`) are served from $PLX_BLOB_DIR/<id> when that is set: save each
//   asset there under its id (Artifact `read` with the id as `path`), else they render missing.
// Each artboard is served next to the runtime as ./support.js and shot at 1920x1080, full page.
const { chromium } = require('playwright');
const http = require('http');
const fs = require('fs');
const path = require('path');

const [src, out, ...names] = process.argv.slice(2);
if (!src || !out) { console.error('usage: render-canvas.js <artifact-files-dir> <out-dir> [Board.dc.html ...]'); process.exit(2); }
const project = path.join(src, 'project');
const runtime = fs.readFileSync(path.join(src, 'artifact-type', 'dc-runtime.js'));
const types = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.ttf': 'font/ttf', '.woff2': 'font/woff2', '.json': 'application/json', '.svg': 'image/svg+xml', '.png': 'image/png', '.jpg': 'image/jpeg' };

const server = http.createServer((req, res) => {
  const rel = decodeURIComponent(req.url.split('?')[0]).replace(/^\/+/, '');
  if (rel === 'support.js') { res.writeHead(200, { 'content-type': 'text/javascript' }); res.end(runtime); return; }
  if (rel.startsWith('_blob/') && process.env.PLX_BLOB_DIR) {
    const id = path.basename(rel);
    const dir = process.env.PLX_BLOB_DIR;
    const hit = fs.readdirSync(dir).find(f => f === id || f.startsWith(id + '.'));
    if (!hit) { res.writeHead(404); res.end(); return; }
    fs.readFile(path.join(dir, hit), (err, body) => {
      if (err) { res.writeHead(404); res.end(); return; }
      const head = body.subarray(0, 4).toString('hex');
      const type = head.startsWith('ffd8') ? 'image/jpeg' : head.startsWith('89504e47') ? 'image/png' : 'application/octet-stream';
      res.writeHead(200, { 'content-type': type }); res.end(body);
    });
    return;
  }
  const file = path.resolve(project, rel);
  if (!file.startsWith(path.resolve(project))) { res.writeHead(403); res.end(); return; }
  fs.readFile(file, (err, body) => {
    if (err) { res.writeHead(404); res.end(); return; }
    res.writeHead(200, { 'content-type': types[path.extname(file)] || 'application/octet-stream' });
    res.end(body);
  });
});

server.listen(0, '127.0.0.1', async () => {
  const port = server.address().port;
  fs.mkdirSync(out, { recursive: true });
  const boards = names.length ? names : fs.readdirSync(project).filter(f => f.endsWith('.dc.html'));
  const browser = await chromium.launch();
  const page = await browser.newPage({ viewport: { width: 1920, height: 1080 } });
  const errors = [];
  page.on('pageerror', e => errors.push(e.message));
  page.on('console', m => { if (m.type() === 'error' && !/Failed to load resource/.test(m.text())) errors.push(m.text()); });
  // A template hole (`src="{{ x }}"`) is fetched once before the runtime fills it; that is not an error.
  page.on('response', r => { if (r.status() >= 400 && !decodeURIComponent(r.url()).includes('{{')) errors.push(r.status() + ' ' + r.url()); });
  let failed = 0;
  for (const board of boards) {
    errors.length = 0;
    await page.goto(`http://127.0.0.1:${port}/${board}`, { waitUntil: 'networkidle' });
    await page.waitForTimeout(1500);
    const png = path.resolve(out, board.replace(/\.dc\.html$/, '.png'));
    await page.screenshot({ path: png, fullPage: true });
    if (errors.length) failed++;
    console.log(`${png}\t${errors.length ? 'ERROR ' + errors.slice(0, 2).join(' | ') : 'ok'}`);
  }
  await browser.close();
  server.close();
  process.exit(failed ? 1 : 0);
});
