'use strict';

const fs = require('fs');
const path = require('path');
const harness = require('../test/harness');

function collectFiles(target) {
  const resolved = path.resolve(target);
  if (!fs.existsSync(resolved)) {
    throw new Error(`Test path not found: ${target}`);
  }
  const stat = fs.statSync(resolved);
  if (stat.isFile()) {
    return [resolved];
  }
  const files = [];
  for (const entry of fs.readdirSync(resolved).sort()) {
    const full = path.join(resolved, entry);
    const entryStat = fs.statSync(full);
    if (entryStat.isDirectory()) {
      files.push(...collectFiles(full));
    } else if (entry.endsWith('.test.js')) {
      files.push(full);
    }
  }
  return files;
}

async function main() {
  const targets = process.argv.slice(2);
  const inputTargets = targets.length > 0 ? targets : ['test/unit', 'test/integration'];
  const files = [];
  for (const target of inputTargets) {
    for (const file of collectFiles(target)) {
      if (!files.includes(file)) {
        files.push(file);
      }
    }
  }

  if (harness.isBuiltin) {
    throw new Error('当前为内置 node:test 模式，请改用 `node --test <paths>`');
  }

  console.log(`发现 ${files.length} 个测试文件`);
  for (const file of files) {
    require(file);
  }
  const code = await harness.run();
  process.exitCode = code;
}

main().catch((err) => {
  console.error(err && err.stack ? err.stack : err);
  process.exitCode = 1;
});
