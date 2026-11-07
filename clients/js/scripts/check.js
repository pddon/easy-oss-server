'use strict';

/**
 * 零依赖“构建”检查：语法检查所有源码/测试/脚本 + 校验发布所需文件存在。
 * TypeScript 用户可另外运行 `npx tsc --noEmit`（本包不强制依赖 typescript）。
 */

const fs = require('fs');
const path = require('path');
const childProcess = require('child_process');

const root = path.resolve(__dirname, '..');
const targets = [
  'index.js',
  'index.mjs',
  'lib/util.js',
  'lib/errors.js',
  'lib/xml.js',
  'lib/signature.js',
  'lib/http.js',
  'lib/streams.js',
  'lib/client.js',
  'scripts/check.js',
  'scripts/run-tests.js',
  'test/harness.js',
];

function walk(dir) {
  const files = [];
  for (const entry of fs.readdirSync(path.join(root, dir))) {
    if (entry.endsWith('.test.js')) {
      files.push(path.join(dir, entry));
    }
  }
  return files;
}

for (const dir of ['test/unit', 'test/integration', 'test/helpers']) {
  if (fs.existsSync(path.join(root, dir))) {
    targets.push(...walk(dir));
  }
}

const required = ['package.json', 'index.js', 'index.mjs', 'types/index.d.ts'];

let failed = 0;
for (const file of required) {
  if (!fs.existsSync(path.join(root, file))) {
    console.error(`缺少必需文件: ${file}`);
    failed += 1;
  }
}
for (const file of targets) {
  const full = path.join(root, file);
  if (!fs.existsSync(full)) {
    console.error(`缺少源码文件: ${file}`);
    failed += 1;
    continue;
  }
  try {
    childProcess.execFileSync(process.execPath, ['--check', full], { stdio: 'pipe' });
  } catch (err) {
    console.error(`语法检查失败: ${file}\n${err.stderr ? err.stderr.toString() : err.message}`);
    failed += 1;
  }
}

if (failed > 0) {
  console.error(`\n构建检查失败：${failed} 项`);
  process.exitCode = 1;
} else {
  console.log(`构建检查通过：${targets.length} 个文件语法正确，必需文件齐全`);
}
