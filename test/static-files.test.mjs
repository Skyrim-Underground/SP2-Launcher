// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from 'node:assert/strict';
import { mkdtemp, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';

import { startServer } from './spawn.mjs';

const CONTENT = Buffer.from('0123456789abcdef'.repeat(64));

async function withRoot() {
  const root = await mkdtemp(join(tmpdir(), 'sug-launcher-static-'));
  await writeFile(join(root, 'file.bin'), CONTENT);
  return root;
}

test('files are served whole or by range', async () => {
  const server = await startServer('static-files.mjs', ['--root', await withRoot()]);
  try {
    const whole = await fetch(`${server.origin}/files/file.bin`);
    assert.equal(whole.status, 200);
    assert.equal(whole.headers.get('content-length'), String(CONTENT.length));
    assert.equal(whole.headers.get('accept-ranges'), 'bytes');
    assert.deepEqual(Buffer.from(await whole.arrayBuffer()), CONTENT);
    const tail = await fetch(`${server.origin}/files/file.bin`, { headers: { range: 'bytes=1000-' } });
    assert.equal(tail.status, 206);
    assert.equal(tail.headers.get('content-range'), `bytes 1000-${CONTENT.length - 1}/${CONTENT.length}`);
    assert.deepEqual(Buffer.from(await tail.arrayBuffer()), CONTENT.subarray(1000));
    const past = await fetch(`${server.origin}/files/file.bin`, { headers: { range: `bytes=${CONTENT.length}-` } });
    assert.equal(past.status, 416);
    const missing = await fetch(`${server.origin}/files/other.bin`);
    assert.equal(missing.status, 404);
    const outside = await fetch(`${server.origin}/files/..%2Fsecret`);
    assert.equal(outside.status, 404);
  } finally {
    server.stop();
  }
});

test('a cut transfer announces the full length and stops early', async () => {
  const server = await startServer('static-files.mjs', ['--root', await withRoot()]);
  try {
    const cut = await fetch(`${server.origin}/cut/100/file.bin`);
    assert.equal(cut.status, 200);
    assert.equal(cut.headers.get('content-length'), String(CONTENT.length));
    await assert.rejects(cut.arrayBuffer());
  } finally {
    server.stop();
  }
});

test('an overrun keeps sending past the file', async () => {
  const server = await startServer('static-files.mjs', ['--root', await withRoot()]);
  try {
    const response = await fetch(`${server.origin}/overrun/file.bin`);
    assert.equal(response.status, 200);
    assert.equal(response.headers.get('content-length'), null);
    const reader = response.body?.getReader();
    assert.ok(reader);
    let received = 0;
    while (received < CONTENT.length + 1) {
      const { value, done } = await reader.read();
      assert.equal(done, false);
      received += value?.length ?? 0;
    }
    await reader.cancel();
  } finally {
    server.stop();
  }
});

test('redirects point where each route says', async () => {
  const server = await startServer('static-files.mjs', [
    '--root',
    await withRoot(),
    '--redirect-to',
    'http://127.0.0.1:1',
  ]);
  try {
    const moved = await fetch(`${server.origin}/moved/file.bin`, { redirect: 'manual' });
    assert.equal(moved.status, 302);
    assert.equal(moved.headers.get('location'), '/files/file.bin');
    const elsewhere = await fetch(`${server.origin}/elsewhere/file.bin`, { redirect: 'manual' });
    assert.equal(elsewhere.headers.get('location'), 'http://127.0.0.1:1/files/file.bin');
    const loop = await fetch(`${server.origin}/loop/file.bin`, { redirect: 'manual' });
    assert.equal(loop.headers.get('location'), '/loop/file.bin');
  } finally {
    server.stop();
  }
});
