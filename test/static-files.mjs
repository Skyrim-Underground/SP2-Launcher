// SPDX-License-Identifier: AGPL-3.0-or-later
// A static file server for download tests: Range requests, plus routes that cut a transfer short, never stop or redirect.
// Prints "listening http://127.0.0.1:<port>" once it accepts connections.
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { resolve, sep } from 'node:path';
import { parseArgs } from 'node:util';

const { values: options } = parseArgs({
  options: {
    root: { type: 'string' },
    port: { type: 'string', default: '0' },
    'redirect-to': { type: 'string' },
  },
});

if (!options.root) {
  console.error('--root <folder> is required');
  process.exit(2);
}
const root = resolve(options.root);
const PADDING = Buffer.alloc(64 * 1024, 'x');

/**
 * The file under the root a route names, or null when the path would leave the root.
 * @param {string} rest
 * @returns {string | null}
 */
function fileUnderRoot(rest) {
  const target = resolve(root, decodeURIComponent(rest));
  return target.startsWith(root + sep) ? target : null;
}

/**
 * @param {import('node:http').ServerResponse} response
 * @param {number} code
 * @param {Record<string, string | number>} headers
 */
function head(response, code, headers) {
  response.writeHead(code, { 'accept-ranges': 'bytes', ...headers });
}

/**
 * The bytes a Range header asks for, or null for a range past the end.
 * @param {string | undefined} range
 * @param {Buffer} bytes
 * @returns {{ start: number, slice: Buffer } | null}
 */
function ranged(range, bytes) {
  const match = /^bytes=(\d+)-(\d*)$/.exec(range ?? '');
  if (!match) {
    return { start: 0, slice: bytes };
  }
  const start = Number(match[1]);
  const end = match[2] === '' ? bytes.length - 1 : Math.min(Number(match[2]), bytes.length - 1);
  if (start >= bytes.length || start > end) {
    return null;
  }
  return { start, slice: bytes.subarray(start, end + 1) };
}

/**
 * Serves the file with Range support; with `cutAfter` the connection is destroyed after that many body bytes.
 * @param {import('node:http').IncomingMessage} request
 * @param {import('node:http').ServerResponse} response
 * @param {string} rest
 * @param {number | null} cutAfter
 */
async function serve(request, response, rest, cutAfter) {
  const target = fileUnderRoot(rest);
  let bytes;
  try {
    bytes = await readFile(target ?? '');
  } catch {
    head(response, 404, {});
    response.end('not found');
    return;
  }
  const part = ranged(request.headers.range, bytes);
  if (part === null) {
    head(response, 416, { 'content-range': `bytes */${bytes.length}` });
    response.end();
    return;
  }
  const headers = { 'content-length': part.slice.length, 'content-type': 'application/octet-stream' };
  if (part.start > 0 || part.slice.length < bytes.length) {
    head(response, 206, {
      ...headers,
      'content-range': `bytes ${part.start}-${part.start + part.slice.length - 1}/${bytes.length}`,
    });
  } else {
    head(response, 200, headers);
  }
  if (cutAfter === null) {
    response.end(part.slice);
    return;
  }
  response.write(part.slice.subarray(0, cutAfter), () => response.destroy());
}

/**
 * Sends the file and then padding without end, so only a client that stops at its stated size ever returns.
 * @param {import('node:http').ServerResponse} response
 * @param {string} rest
 */
async function overrun(response, rest) {
  const target = fileUnderRoot(rest);
  let bytes;
  try {
    bytes = await readFile(target ?? '');
  } catch {
    head(response, 404, {});
    response.end('not found');
    return;
  }
  head(response, 200, { 'content-type': 'application/octet-stream' });
  response.write(bytes);
  const pump = () => {
    while (!response.destroyed && response.write(PADDING)) {
      // keep writing until the socket applies back pressure or closes
    }
  };
  response.on('drain', pump);
  pump();
}

const http = createServer((request, response) => {
  if (request.method !== 'GET') {
    head(response, 405, {});
    response.end();
    return;
  }
  const path = new URL(request.url ?? '/', 'http://127.0.0.1').pathname;
  const cut = /^\/cut\/(\d+)\/(.+)$/.exec(path);
  if (cut) {
    void serve(request, response, cut[2], Number(cut[1]));
    return;
  }
  const [, route, rest] = /^\/([a-z]+)\/(.+)$/.exec(path) ?? [];
  switch (route) {
    case 'files':
      void serve(request, response, rest, null);
      return;
    case 'overrun':
      void overrun(response, rest);
      return;
    case 'moved':
      head(response, 302, { location: `/files/${rest}` });
      response.end();
      return;
    case 'elsewhere':
      if (!options['redirect-to']) {
        head(response, 404, {});
        response.end();
        return;
      }
      head(response, 302, { location: `${options['redirect-to']}/files/${rest}` });
      response.end();
      return;
    case 'loop':
      head(response, 302, { location: `/loop/${rest}` });
      response.end();
      return;
    default:
      head(response, 404, {});
      response.end('not found');
  }
});

http.listen(Number(options.port), '127.0.0.1', () => {
  const address = http.address();
  const port = typeof address === 'object' && address ? address.port : options.port;
  console.log(`listening http://127.0.0.1:${port}`);
});
