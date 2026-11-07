'use strict';

/**
 * 集成测试用服务端实例管理：
 * 1. 优先使用 target/release/oss-server.exe，不存在则用 cargo 构建（独立 CARGO_TARGET_DIR）；
 * 2. 固定监听 127.0.0.1:18102，数据目录 $TEMP/oss-server-js-data；
 * 3. 等 /healthz 就绪后通过管理 API 创建 App，返回 accessKey/secretKey；
 * 4. 测试结束 stop() 杀掉进程，不留后台任务。
 */

const childProcess = require('child_process');
const fs = require('fs');
const http = require('http');
const net = require('net');
const os = require('os');
const path = require('path');

const PORT = 18102;
const HOST = '127.0.0.1';
const ADMIN_TOKEN = 'js-client-test-admin-token';
const DATA_DIR = path.join(process.env.TEMP || os.tmpdir(), 'oss-server-js-data');
const REPO_ROOT = path.resolve(__dirname, '../../../..');
const BUILD_DIR = path.join(process.env.TEMP || os.tmpdir(), 'oss-server-build-js');

function requestJson(options) {
  return new Promise((resolve, reject) => {
    const url = new URL(options.url);
    const body = options.body ? Buffer.from(JSON.stringify(options.body)) : null;
    const req = http.request(
      url,
      {
        method: options.method || 'GET',
        headers: Object.assign(
          { host: url.host },
          body ? { 'content-type': 'application/json', 'content-length': body.length } : {},
          options.headers || {}
        ),
      },
      (res) => {
        const chunks = [];
        res.on('data', (chunk) => chunks.push(Buffer.from(chunk)));
        res.on('end', () => {
          const text = Buffer.concat(chunks).toString('utf8');
          resolve({ statusCode: res.statusCode, headers: res.headers, text });
        });
      }
    );
    req.on('error', reject);
    req.end(body || undefined);
  });
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function isPortOpen(port) {
  return new Promise((resolve) => {
    const socket = net.connect({ port, host: HOST });
    const done = (result) => {
      socket.destroy();
      resolve(result);
    };
    socket.setTimeout(500);
    socket.once('connect', () => done(true));
    socket.once('timeout', () => done(false));
    socket.once('error', () => done(false));
  });
}

function rmDir(dir) {
  fs.rmSync(dir, { recursive: true, force: true });
}

function cargoCandidates() {
  const candidates = [];
  if (process.env.CARGO) {
    candidates.push(process.env.CARGO);
  }
  candidates.push(path.join(os.homedir(), '.cargo', 'bin', 'cargo.exe'));
  candidates.push(
    path.join(os.homedir(), '.rustup', 'toolchains', 'stable-x86_64-pc-windows-gnu', 'bin', 'cargo.exe')
  );
  candidates.push('cargo');
  return candidates;
}

function buildServer() {
  const targetDir = BUILD_DIR;
  fs.mkdirSync(targetDir, { recursive: true });
  for (const candidate of cargoCandidates()) {
    let versionCheck;
    try {
      versionCheck = childProcess.spawnSync(candidate, ['--version'], { stdio: 'pipe' });
    } catch (err) {
      continue;
    }
    if (versionCheck.status !== 0) {
      continue;
    }
    console.log(`预编译二进制缺失或过期，使用 cargo 构建：${candidate}（CARGO_TARGET_DIR=${targetDir}）`);
    const result = childProcess.spawnSync(candidate, ['build', '--release'], {
      cwd: REPO_ROOT,
      env: Object.assign({}, process.env, {
        CARGO_TARGET_DIR: targetDir,
        // 直接调用 toolchain 内的 cargo 时需要同目录的 rustc
        PATH: `${path.dirname(candidate)};${process.env.PATH || ''}`,
      }),
      stdio: 'inherit',
    });
    if (result.status !== 0) {
      throw new Error(`cargo build --release 失败（exit=${result.status}）`);
    }
    return path.join(targetDir, 'release', 'oss-server.exe');
  }
  throw new Error('未找到 oss-server.exe，且无法调用 cargo 构建');
}

/**
 * 源码最新修改时间。只有比它新的二进制才可用：
 * 服务端源码更新后旧的 target/release 二进制不再包含新逻辑（例如 S3 鉴权），
 * 继续使用会得到与源码不一致的行为。
 */
function newestSourceMtime() {
  let newest = 0;
  const statFile = (file) => {
    try {
      const mtime = fs.statSync(file).mtimeMs;
      if (mtime > newest) {
        newest = mtime;
      }
    } catch (err) {
      // ignore
    }
  };
  const walk = (dir) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        walk(full);
      } else if (entry.name.endsWith('.rs')) {
        statFile(full);
      }
    }
  };
  walk(path.join(REPO_ROOT, 'src'));
  statFile(path.join(REPO_ROOT, 'Cargo.toml'));
  statFile(path.join(REPO_ROOT, 'Cargo.lock'));
  return newest;
}

function resolveBinary() {
  const sourceMtime = newestSourceMtime();
  const candidates = [
    { file: path.join(REPO_ROOT, 'target', 'release', 'oss-server.exe'), source: 'prebuilt-release' },
    { file: path.join(REPO_ROOT, 'target', 'debug', 'oss-server.exe'), source: 'prebuilt-debug' },
  ];
  for (const candidate of candidates) {
    if (!fs.existsSync(candidate.file)) {
      continue;
    }
    if (fs.statSync(candidate.file).mtimeMs >= sourceMtime) {
      return { binary: candidate.file, source: candidate.source };
    }
  }
  return { binary: buildServer(), source: 'cargo' };
}

async function waitForHealth(timeoutMs, child) {
  const deadline = Date.now() + timeoutMs;
  let lastError = null;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) {
      throw new Error(`服务端进程提前退出，exitCode=${child.exitCode}`);
    }
    try {
      const res = await requestJson({ url: `http://${HOST}:${PORT}/healthz` });
      if (res.statusCode === 200) {
        return JSON.parse(res.text);
      }
      lastError = new Error(`/healthz 返回 ${res.statusCode}: ${res.text}`);
    } catch (err) {
      lastError = err;
    }
    await sleep(200);
  }
  throw new Error(`等待服务端就绪超时：${lastError ? lastError.message : 'unknown'}`);
}

async function createApp() {
  const res = await requestJson({
    url: `http://${HOST}:${PORT}/api/admin/apps`,
    method: 'POST',
    headers: { 'x-admin-token': ADMIN_TOKEN },
    body: { name: `js-client-${Date.now().toString(36)}` },
  });
  if (res.statusCode !== 200 && res.statusCode !== 201) {
    throw new Error(`创建 App 失败：HTTP ${res.statusCode} ${res.text}`);
  }
  const data = JSON.parse(res.text);
  if (!data.app_id || !data.app_secret) {
    throw new Error(`创建 App 响应缺少 app_id/app_secret：${res.text}`);
  }
  return { accessKey: data.app_id, secretKey: data.app_secret, raw: data };
}

async function startServer() {
  if (await isPortOpen(PORT)) {
    throw new Error(
      `端口 ${PORT} 已被占用。集成测试需要独占该端口，请先停止占用进程后重跑。`
    );
  }

  rmDir(DATA_DIR);
  fs.mkdirSync(DATA_DIR, { recursive: true });
  const { binary, source } = resolveBinary();

  const logFile = path.join(DATA_DIR, 'server.log');
  const logStream = fs.createWriteStream(logFile, { flags: 'a' });
  const env = Object.assign({}, process.env, {
    OSS_HOST: `${HOST}:${PORT}`,
    OSS_DATA_DIR: DATA_DIR,
    OSS_ADMIN_TOKEN: ADMIN_TOKEN,
    OSS_MASTER_KEY: '',
    OSS_STORAGE_BACKEND: 'memory',
    OSS_CLEANUP_INTERVAL_MINUTES: '0',
    RUST_LOG: process.env.RUST_LOG || 'info',
  });

  const child = childProcess.spawn(binary, [], {
    cwd: DATA_DIR,
    env,
    stdio: ['ignore', 'pipe', 'pipe'],
    windowsHide: true,
  });
  child.stdout.pipe(logStream);
  child.stderr.pipe(logStream);

  const tail = [];
  const remember = (chunk) => {
    tail.push(chunk.toString());
    if (tail.length > 40) {
      tail.shift();
    }
  };
  child.stdout.on('data', remember);
  child.stderr.on('data', remember);

  const startedAt = Date.now();
  try {
    const health = await waitForHealth(45000, child);
    const app = await createApp();
    return {
      endpoint: `http://${HOST}:${PORT}`,
      accessKey: app.accessKey,
      secretKey: app.secretKey,
      appId: app.accessKey,
      dataDir: DATA_DIR,
      logFile,
      binary,
      binarySource: source,
      health,
      startupMs: Date.now() - startedAt,
      async stop() {
        if (child.exitCode !== null) {
          return;
        }
        await new Promise((resolve) => {
          let settled = false;
          const finish = () => {
            if (!settled) {
              settled = true;
              resolve();
            }
          };
          child.once('exit', finish);
          child.kill();
          setTimeout(() => {
            if (child.exitCode === null) {
              childProcess.spawnSync('taskkill', ['/PID', String(child.pid), '/T', '/F'], {
                stdio: 'ignore',
                windowsHide: true,
              });
            }
          }, 3000).unref();
          setTimeout(finish, 6000).unref();
        });
        logStream.end();
        // 确认端口已释放
        for (let i = 0; i < 20; i += 1) {
          if (!(await isPortOpen(PORT))) {
            return;
          }
          await sleep(150);
        }
      },
      tailLog() {
        return tail.join('');
      },
    };
  } catch (err) {
    try {
      child.kill();
    } catch (killError) {
      // ignore
    }
    throw new Error(`${err.message}\n服务端日志（尾部）：\n${tail.join('')}`);
  }
}

module.exports = { ADMIN_TOKEN, DATA_DIR, PORT, startServer };
