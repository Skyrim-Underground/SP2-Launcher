// SPDX-License-Identifier: AGPL-3.0-or-later
// Starts one of the Node servers in this folder on a free port and resolves once it says where it listens.
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

/**
 * @param {string} script the file name in this folder
 * @param {string[]} args
 * @returns {Promise<{ origin: string, stop: () => void }>}
 */
export function startServer(script, args) {
  const path = fileURLToPath(new URL(`./${script}`, import.meta.url));
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [path, '--port', '0', ...args], {
      stdio: ['ignore', 'pipe', 'inherit'],
    });
    let buffered = '';
    child.stdout.setEncoding('utf8');
    child.stdout.on('data', (chunk) => {
      buffered += chunk;
      const line = buffered.split('\n')[0];
      if (line.startsWith('listening ')) {
        resolve({ origin: line.slice('listening '.length).trim(), stop: () => child.kill() });
      }
    });
    child.on('error', reject);
    child.on('exit', (code) => reject(new Error(`${script} exited with ${code}`)));
  });
}
