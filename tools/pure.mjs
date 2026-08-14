// Loads the DOM-free block of src/index.html so node can use the exact same code the app runs.
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

export const root = join(dirname(fileURLToPath(import.meta.url)), '..');

export function load() {
  const html = readFileSync(join(root, 'src', 'index.html'), 'utf8');
  const A = '/* ==================== PURE START ====================';
  const B = '/* ==================== PURE END ==================== */';
  const a = html.indexOf(A), b = html.indexOf(B);
  if (a < 0 || b < 0) throw new Error('PURE markers missing in src/index.html');
  const module = { exports: {} };
  new Function('module', html.slice(a, b))(module);
  return module.exports;
}
