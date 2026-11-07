'use strict';

/**
 * 构建脚本：分别产出 CommonJS 与 ESM，并生成类型声明。
 *
 * - CJS：dist/cjs/*.js + dist/types/*.d.ts
 * - ESM：dist/esm/*.js（相对导入补 .js 扩展名，并附带 {"type":"module"}）
 */

const childProcess = require('child_process');
const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');
const DIST = path.join(ROOT, 'dist');

function resolveTsc() {
  try {
    return require.resolve('typescript/bin/tsc', { paths: [ROOT] });
  } catch (err) {
    throw new Error(
      '未找到本地 TypeScript 编译器，请先执行 npm install（或确认 node_modules/typescript 存在）'
    );
  }
}

function runTsc(args) {
  const result = childProcess.spawnSync(process.execPath, [resolveTsc(), ...args], {
    cwd: ROOT,
    stdio: 'inherit',
  });
  if (result.status !== 0) {
    throw new Error(`tsc ${args.join(' ')} 失败（exit=${result.status === null ? 'null' : result.status}）`);
  }
}

function walk(dir) {
  const files = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      files.push(...walk(full));
    } else {
      files.push(full);
    }
  }
  return files;
}

/** ESM 下相对导入必须带扩展名，tsc 不会自动补。 */
function addEsmExtensions() {
  const esmDir = path.join(DIST, 'esm');
  if (!fs.existsSync(esmDir)) {
    return;
  }
  for (const file of walk(esmDir)) {
    if (!file.endsWith('.js')) {
      continue;
    }
    const source = fs.readFileSync(file, 'utf8');
    const rewritten = source.replace(
      /(from\s*['"])(\.{1,2}\/[^'"]+?)(['"])/g,
      (match, prefix, specifier, suffix) =>
        specifier.endsWith('.js') ? match : `${prefix}${specifier}.js${suffix}`
    );
    if (rewritten !== source) {
      fs.writeFileSync(file, rewritten, 'utf8');
    }
  }
  fs.writeFileSync(path.join(esmDir, 'package.json'), '{"type":"module"}\n', 'utf8');
  fs.writeFileSync(path.join(DIST, 'cjs', 'package.json'), '{"type":"commonjs"}\n', 'utf8');
}

function main() {
  const arg = process.argv[2] || '';
  if (arg === '--clean') {
    fs.rmSync(DIST, { recursive: true, force: true });
    console.log('已清理 dist/');
    return;
  }
  if (arg === '--check') {
    runTsc(['-p', 'tsconfig.cjs.json', '--noEmit']);
    console.log('类型检查通过（tsc --noEmit, strict）');
    return;
  }

  fs.rmSync(DIST, { recursive: true, force: true });
  console.log('构建 CommonJS + 类型声明 ...');
  runTsc(['-p', 'tsconfig.cjs.json']);
  console.log('构建 ESM ...');
  runTsc(['-p', 'tsconfig.esm.json']);
  addEsmExtensions();
  console.log('构建完成：dist/cjs（CommonJS）、dist/esm（ESM）、dist/types（.d.ts）');
}

try {
  main();
} catch (err) {
  console.error(err && err.message ? err.message : err);
  process.exit(1);
}
