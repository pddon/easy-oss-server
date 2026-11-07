'use strict';

/**
 * 零依赖测试 runner（Node 14 无 node:test，故自带 describe/it/assert 风格）。
 * 支持 before/after/beforeEach/afterEach、逐用例隔离与超时。
 */

const assert = require('assert');

const rootSuite = {
  name: '',
  tests: [],
  suites: [],
  beforeAll: [],
  afterAll: [],
  beforeEach: [],
  afterEach: [],
};

let currentSuite = rootSuite;

function describe(name, fn) {
  const parent = currentSuite;
  const suite = {
    name,
    tests: [],
    suites: [],
    beforeAll: [],
    afterAll: [],
    beforeEach: [],
    afterEach: [],
  };
  parent.suites.push(suite);
  currentSuite = suite;
  try {
    fn();
  } finally {
    currentSuite = parent;
  }
}

function it(name, fn) {
  currentSuite.tests.push({ name, fn });
}

function before(fn) {
  currentSuite.beforeAll.push(fn);
}

function after(fn) {
  currentSuite.afterAll.push(fn);
}

function beforeEach(fn) {
  currentSuite.beforeEach.push(fn);
}

function afterEach(fn) {
  currentSuite.afterEach.push(fn);
}

function withTimeout(promise, ms, label) {
  if (!ms) {
    return promise;
  }
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`Timeout ${ms}ms: ${label}`)), ms);
    if (typeof timer.unref === 'function') {
      timer.unref();
    }
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (err) => {
        clearTimeout(timer);
        reject(err);
      }
    );
  });
}

let passed = 0;
let failed = 0;

async function runSuite(suite, inherited, pathParts, options) {
  const path = pathParts.concat(suite.name ? [suite.name] : []);
  const beforeAll = inherited.beforeAll.concat(suite.beforeAll);
  const beforeEach = inherited.beforeEach.concat(suite.beforeEach);
  const afterEach = suite.afterEach.concat(inherited.afterEach || []);

  for (const hook of suite.beforeAll) {
    await withTimeout(Promise.resolve(hook()), options.timeout, `${path.join(' > ')} before`);
  }

  try {
    for (const test of suite.tests) {
      const label = path.concat(test.name).join(' > ');
      const started = Date.now();
      try {
        for (const hook of beforeEach) {
          await withTimeout(Promise.resolve(hook()), options.timeout, `${label} beforeEach`);
        }
        await withTimeout(Promise.resolve(test.fn()), options.timeout, label);
        passed += 1;
        console.log(`  ok   ${label} (${Date.now() - started}ms)`);
      } catch (err) {
        failed += 1;
        console.error(`  FAIL ${label} (${Date.now() - started}ms)`);
        console.error(`       ${err && err.stack ? err.stack : err}`);
      } finally {
        for (const hook of afterEach) {
          try {
            await withTimeout(Promise.resolve(hook()), options.timeout, `${label} afterEach`);
          } catch (err) {
            failed += 1;
            console.error(`  FAIL ${label} afterEach: ${err && err.stack ? err.stack : err}`);
          }
        }
      }
    }
    for (const child of suite.suites) {
      await runSuite(child, { beforeAll, beforeEach, afterEach }, path, options);
    }
  } finally {
    for (const hook of suite.afterAll) {
      try {
        await withTimeout(Promise.resolve(hook()), options.timeout, `${path.join(' > ')} after`);
      } catch (err) {
        failed += 1;
        console.error(`  FAIL ${path.join(' > ')} after: ${err && err.stack ? err.stack : err}`);
      }
    }
  }
}

function autorun(options) {
  const opts = Object.assign({ timeout: 30000 }, options || {});
  console.log(`\n${opts.title || '测试'}`);
  runSuite(rootSuite, { beforeAll: [], beforeEach: [], afterEach: [] }, [], opts).then(
    () => {
      console.log(`\n${opts.title || '测试'}：通过 ${passed}，失败 ${failed}`);
      process.exitCode = failed === 0 ? 0 : 1;
    },
    (err) => {
      console.error(err && err.stack ? err.stack : err);
      process.exitCode = 1;
    }
  );
}

module.exports = {
  assert,
  describe,
  it,
  before,
  after,
  beforeEach,
  afterEach,
  autorun,
};
