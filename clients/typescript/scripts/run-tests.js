'use strict';

const childProcess = require('child_process');
const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');

const SUITES = {
  unit: ['test/unit/signature.test.js', 'test/unit/client.test.js'],
  integration: ['test/integration/integration.test.js'],
};

function runFile(relative) {
  const file = path.join(ROOT, relative);
  if (!fs.existsSync(file)) {
    throw new Error(`测试文件不存在：${relative}`);
  }
  console.log(`\n=== ${relative} ===`);
  const result = childProcess.spawnSync(process.execPath, [file], { cwd: ROOT, stdio: 'inherit' });
  if (result.status !== 0) {
    throw new Error(`${relative} 失败（exit=${result.status === null ? 'null' : result.status}）`);
  }
}

function main() {
  const target = process.argv[2] || 'all';
  const suites = target === 'all' ? ['unit', 'integration'] : [target];
  if (!SUITES[target] && target !== 'all') {
    throw new Error(`未知测试套件：${target}（可选 unit / integration / all）`);
  }
  for (const suite of suites) {
    for (const file of SUITES[suite]) {
      runFile(file);
    }
  }
}

try {
  main();
} catch (err) {
  console.error(err && err.message ? err.message : err);
  process.exit(1);
}
