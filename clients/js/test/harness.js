'use strict';

/**
 * 测试宿主。
 *
 * 本机 Node 为 14.x，没有内置 `node:test`，因此这里实现了一个 API 兼容的
 * 极简 runner（describe / it / before / after / beforeEach / afterEach）。
 * 在 Node >= 18 时设置环境变量 `OSS_TEST_NODE_RUNNER=1` 会直接委托给
 * 内置 `node:test`（用 `node --test` 运行同一批文件）。
 */

let builtin = null;
if (process.env.OSS_TEST_NODE_RUNNER === '1') {
  try {
    // eslint-disable-next-line node/no-unsupported-features/node-builtins
    builtin = require('node:test');
  } catch (err) {
    builtin = null;
  }
}

if (builtin) {
  module.exports = {
    isBuiltin: true,
    describe: builtin.describe,
    it: builtin.it,
    test: builtin.it,
    before: builtin.before,
    after: builtin.after,
    beforeEach: builtin.beforeEach,
    afterEach: builtin.afterEach,
    autorun() {},
    run: async () => 0,
  };
} else {
  const root = {
    name: '',
    tests: [],
    suites: [],
    before: [],
    after: [],
    beforeEach: [],
    afterEach: [],
    parent: null,
  };
  let current = root;
  const loadedFiles = new Set();

  function addHook(name, fn) {
    if (typeof fn !== 'function') {
      throw new TypeError(`${name} requires a function`);
    }
    current[name].push(fn);
  }

  const api = {
    isBuiltin: false,
    describe(name, fn) {
      const suite = {
        name,
        tests: [],
        suites: [],
        before: [],
        after: [],
        beforeEach: [],
        afterEach: [],
        parent: current,
      };
      current.suites.push(suite);
      const previous = current;
      current = suite;
      try {
        fn();
      } finally {
        current = previous;
      }
    },
    it(name, fn) {
      current.tests.push({ name, fn });
    },
    before: (fn) => addHook('before', fn),
    after: (fn) => addHook('after', fn),
    beforeEach: (fn) => addHook('beforeEach', fn),
    afterEach: (fn) => addHook('afterEach', fn),
    autorun() {
      loadedFiles.add(current);
    },
    async run() {
      return runRoot();
    },
  };
  api.test = api.it;

  const stats = { total: 0, passed: 0, failed: 0, skipped: 0, duration: 0 };
  const failures = [];
  const startedAt = Date.now();
  const TEST_TIMEOUT = Number(process.env.OSS_TEST_TIMEOUT_MS || 180000);

  function withTimeout(fn, label) {
    return new Promise((resolve, reject) => {
      let timer = setTimeout(() => {
        reject(new Error(`Test timed out after ${TEST_TIMEOUT}ms: ${label}`));
      }, TEST_TIMEOUT);
      if (timer.unref) {
        timer.unref();
      }
      Promise.resolve()
        .then(fn)
        .then(
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

  function indentFor(depth) {
    return '  '.repeat(depth);
  }

  function collectHooks(suite, key) {
    const chain = [];
    let node = suite;
    while (node) {
      chain.unshift(node[key]);
      node = node.parent;
    }
    return chain.reduce((all, hooks) => all.concat(hooks), []);
  }

  async function runSuite(suite, depth) {
    if (suite.name) {
      console.log(`${indentFor(depth)}▶ ${suite.name}`);
    }
    const beforeHooks = collectHooks(suite, 'before');
    const afterHooks = collectHooks(suite, 'after').reverse();
    const beforeEachHooks = collectHooks(suite, 'beforeEach');
    const afterEachHooks = collectHooks(suite, 'afterEach').reverse();

    for (const hook of beforeHooks) {
      await withTimeout(() => hook(), `before hook of ${suite.name || 'root'}`);
    }
    try {
      for (const test of suite.tests) {
        stats.total += 1;
        const testStarted = Date.now();
        const label = `${suite.name} > ${test.name}`;
        try {
          for (const hook of beforeEachHooks) {
            await withTimeout(() => hook(), `beforeEach hook of ${label}`);
          }
          await withTimeout(() => test.fn(), label);
          for (const hook of afterEachHooks) {
            await withTimeout(() => hook(), `afterEach hook of ${label}`);
          }
          stats.passed += 1;
          console.log(`${indentFor(depth + 1)}✓ ${test.name} (${Date.now() - testStarted}ms)`);
        } catch (err) {
          stats.failed += 1;
          failures.push({ label, error: err });
          console.log(`${indentFor(depth + 1)}✗ ${test.name} (${Date.now() - testStarted}ms)`);
        }
      }
      for (const child of suite.suites) {
        await runSuite(child, depth + 1);
      }
    } finally {
      for (const hook of afterHooks) {
        try {
          await withTimeout(() => hook(), `after hook of ${suite.name || 'root'}`);
        } catch (err) {
          stats.failed += 1;
          failures.push({ label: `after hook ${suite.name || 'root'}`, error: err });
        }
      }
    }
  }

  async function runRoot() {
    await runSuite(root, 0);
    stats.duration = Date.now() - startedAt;
    console.log('');
    if (failures.length > 0) {
      console.log('失败详情：');
      for (const failure of failures) {
        console.log(`- ${failure.label}`);
        const message = failure.error && (failure.error.stack || failure.error.message);
        console.log(String(message).split('\n').slice(0, 8).map((line) => `  ${line}`).join('\n'));
      }
      console.log('');
    }
    console.log(
      `结果：共 ${stats.total}，通过 ${stats.passed}，失败 ${stats.failed}，耗时 ${stats.duration}ms`
    );
    return stats.failed === 0 ? 0 : 1;
  }

  module.exports = api;
}
