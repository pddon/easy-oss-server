'use strict';

const { Buffer } = require('buffer');

/**
 * 把 Readable 包装成可“暂停/恢复”的异步读取器：
 * 只在下游需要时才 resume，避免 for-await 提前 break 时销毁用户流。
 */
function createChunkReader(stream) {
  const queue = [];
  let waiter = null;
  let ended = false;
  let failure = null;
  let paused = false;

  const settle = (chunk) => {
    if (waiter) {
      const current = waiter;
      waiter = null;
      current.resolve({ value: chunk, done: false });
    } else {
      queue.push(chunk);
      if (!paused) {
        stream.pause();
        paused = true;
      }
    }
  };

  stream.on('data', (chunk) => {
    if (failure) {
      return;
    }
    settle(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
  });
  stream.on('end', () => {
    ended = true;
    if (waiter) {
      const current = waiter;
      waiter = null;
      current.resolve({ value: undefined, done: true });
    }
  });
  stream.on('error', (err) => {
    failure = err;
    if (waiter) {
      const current = waiter;
      waiter = null;
      current.reject(err);
    }
  });

  return {
    async next() {
      if (failure) {
        throw failure;
      }
      if (queue.length > 0) {
        const value = queue.shift();
        if (paused) {
          stream.resume();
          paused = false;
        }
        return { value, done: false };
      }
      if (ended) {
        return { value: undefined, done: true };
      }
      return new Promise((resolve, reject) => {
        waiter = { resolve, reject };
      });
    },
    [Symbol.asyncIterator]() {
      return this;
    },
  };
}

/** 把任意 Buffer / 异步 chunk 来源切成不超过 partSize 的分片。 */
async function* iterInParts(source, partSize) {
  const chunks = [];
  let pending = 0;

  const flush = function* flushParts() {
    while (pending >= partSize) {
      const joined = Buffer.concat(chunks);
      yield joined.subarray(0, partSize);
      const rest = joined.subarray(partSize);
      chunks.length = 0;
      if (rest.length > 0) {
        chunks.push(rest);
      }
      pending = rest.length;
    }
  };

  for await (const chunk of source) {
    if (!chunk || chunk.length === 0) {
      continue;
    }
    chunks.push(chunk);
    pending += chunk.length;
    if (pending >= partSize) {
      yield* flush();
    }
  }
  if (pending > 0) {
    yield Buffer.concat(chunks);
  }
}

/** 将 Buffer 视为可迭代的 chunk 来源。 */
async function* bufferChunks(buffer) {
  yield buffer;
}

module.exports = { bufferChunks, createChunkReader, iterInParts };
