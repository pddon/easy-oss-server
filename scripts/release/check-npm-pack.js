#!/usr/bin/env node
'use strict';

/**
 * 校验 npm 包的发布内容与元数据。
 * 用法：node check-npm-pack.js <包目录> <允许的路径前缀或精确文件名>...
 * 退出码非 0 表示存在不应发布的文件或缺少必需字段。
 */

const childProcess = require('child_process');
const fs = require('fs');
const path = require('path');

const pkgDir = path.resolve(process.argv[2] || '.');
const allowed = process.argv.slice(3);
if (allowed.length === 0) {
  console.error('用法: node check-npm-pack.js <包目录> <允许的路径...>');
  process.exit(2);
}

const pkg = JSON.parse(fs.readFileSync(path.join(pkgDir, 'package.json'), 'utf8'));
const errors = [];

for (const field of ['name', 'version', 'description', 'license']) {
  if (!pkg[field]) {
    errors.push(`package.json 缺少 ${field}`);
  }
}
if (!pkg.repository || !pkg.repository.url) {
  errors.push('package.json 缺少 repository.url');
}
if (!pkg.homepage) {
  errors.push('package.json 缺少 homepage');
}
if (!pkg.bugs || !pkg.bugs.url) {
  errors.push('package.json 缺少 bugs.url');
}
if (!pkg.publishConfig || pkg.publishConfig.access !== 'public') {
  errors.push('package.json 缺少 publishConfig.access=public');
}
const strict = process.env.RELEASE_STRICT === '1';
const placeholderValues = [pkg.name, pkg.description, pkg.repository && pkg.repository.url].filter(
  (value) => value && value.includes('__')
);
if (placeholderValues.length > 0) {
  const message = `package.json 中仍有未替换的占位符: ${placeholderValues.join(', ')}`;
  if (strict) {
    errors.push(message);
  } else {
    console.log(`[警告] ${message}（设置 RELEASE_STRICT=1 视为失败）`);
  }
}

const entries = [pkg.main, pkg.module, pkg.types].filter(Boolean);
for (const entry of entries) {
  const target = path.join(pkgDir, entry);
  if (!fs.existsSync(target)) {
    errors.push(`入口文件不存在: ${entry}（请先执行 npm run build）`);
  }
}

const npmCommand = process.platform === 'win32' ? 'npm.cmd' : 'npm';
const pack = childProcess.spawnSync(npmCommand, ['pack', '--dry-run', '--json'], {
  cwd: pkgDir,
  encoding: 'utf8',
  shell: false,
});
if (pack.error) {
  console.error(`无法执行 npm pack: ${pack.error.message}`);
  process.exit(1);
}
if (pack.status !== 0) {
  console.error(pack.stderr || pack.stdout);
  process.exit(pack.status || 1);
}

let report;
try {
  report = JSON.parse(pack.stdout);
} catch (err) {
  console.error('无法解析 npm pack --json 输出：');
  console.error(pack.stdout);
  process.exit(1);
}

const files = (report[0].files || []).map((f) => f.path);
const unexpected = files.filter((file) => {
  if (file === 'package.json' || file === 'README.md' || file === 'LICENSE') {
    return false;
  }
  return !allowed.some((prefix) => file === prefix || file.startsWith(`${prefix}/`));
});

console.log(`[npm pack] ${pkg.name}@${pkg.version}，共 ${files.length} 个文件，约 ${Math.round(report[0].size / 1024)} KB`);
for (const file of files) {
  console.log(`  - ${file}`);
}

if (unexpected.length > 0) {
  errors.push(`以下文件不在白名单内: ${unexpected.join(', ')}`);
}

if (errors.length > 0) {
  console.error('[失败]');
  for (const error of errors) {
    console.error(`  - ${error}`);
  }
  process.exit(1);
}

console.log('[通过] 包内容与元数据符合发布要求');
