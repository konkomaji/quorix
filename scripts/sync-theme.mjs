// SPDX-License-Identifier: GPL-3.0-or-later
// Copies theme/ into every product that ships it. Run after editing theme/quorix.css.
import { cpSync, rmSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const targets = ['website/assets/theme', 'extensions/chromium/theme'];

for (const t of targets) {
  const dest = join(root, t);
  rmSync(dest, { recursive: true, force: true });
  cpSync(join(root, 'theme'), dest, { recursive: true });
  console.log('synced', t);
}
