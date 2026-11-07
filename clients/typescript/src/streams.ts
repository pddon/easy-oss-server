import * as crypto from 'crypto';
import { Readable } from 'stream';

/** 把流读成 Buffer（仅用于错误响应等小体量场景）。 */
export function readStreamToBuffer(stream: Readable): Promise<Buffer> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    stream.on('data', (chunk: Buffer | string) => chunks.push(Buffer.from(chunk)));
    stream.on('end', () => resolve(Buffer.concat(chunks)));
    stream.on('error', reject);
  });
}

/** 流式计算 MD5（base64），用于文件上传前预校验。 */
export function md5StreamBase64(stream: Readable): Promise<string> {
  return new Promise((resolve, reject) => {
    const hash = crypto.createHash('md5');
    stream.on('data', (chunk: Buffer | string) => hash.update(chunk));
    stream.on('end', () => resolve(hash.digest('base64')));
    stream.on('error', reject);
  });
}

/** 把 Readable 转成 Buffer 异步迭代器，避免把流整体读入内存。 */
export async function* readChunks(stream: Readable): AsyncGenerator<Buffer> {
  for await (const chunk of stream as AsyncIterable<Buffer | string>) {
    if (Buffer.isBuffer(chunk)) {
      yield chunk;
    } else {
      yield Buffer.from(chunk);
    }
  }
}

/** 将 Buffer 视为可迭代的 chunk 来源。 */
export async function* bufferChunks(buffer: Buffer): AsyncGenerator<Buffer> {
  yield buffer;
}

/** 把任意 chunk 来源切成不超过 partSize 的分片（最后一片可小于 partSize）。 */
export async function* iterInParts(
  source: AsyncIterable<Buffer>,
  partSize: number
): AsyncGenerator<Buffer> {
  let pending: Buffer[] = [];
  let pendingBytes = 0;

  for await (const chunk of source) {
    if (!chunk || chunk.length === 0) {
      continue;
    }
    pending.push(chunk);
    pendingBytes += chunk.length;
    if (pendingBytes < partSize) {
      continue;
    }
    let joined = pending.length === 1 ? pending[0] : Buffer.concat(pending, pendingBytes);
    while (joined.length >= partSize) {
      yield joined.subarray(0, partSize);
      joined = joined.subarray(partSize);
    }
    pending = joined.length > 0 ? [joined] : [];
    pendingBytes = joined.length;
  }

  if (pendingBytes > 0) {
    yield pending.length === 1 ? pending[0] : Buffer.concat(pending, pendingBytes);
  }
}

/**
 * 把 Readable 包装成可按需拉取的异步迭代器（下游不拉取时暂停源流）。
 * 供需要对流做背压控制的场景使用。
 */
export function createChunkReader(stream: Readable): AsyncIterable<Buffer> {
  const queue: Buffer[] = [];
  let waiter: {
    resolve: (result: IteratorResult<Buffer>) => void;
    reject: (error: Error) => void;
  } | null = null;
  let ended = false;
  let failure: Error | null = null;
  let paused = false;

  const settle = (chunk: Buffer): void => {
    if (waiter) {
      const current = waiter;
      waiter = null;
      current.resolve({ value: chunk, done: false });
      return;
    }
    queue.push(chunk);
    if (!paused) {
      stream.pause();
      paused = true;
    }
  };

  stream.on('data', (chunk: Buffer | string) => {
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
  stream.on('error', (err: Error) => {
    failure = err;
    if (waiter) {
      const current = waiter;
      waiter = null;
      current.reject(err);
    }
  });

  const iterator: AsyncIterableIterator<Buffer> = {
    next(): Promise<IteratorResult<Buffer>> {
      if (failure) {
        return Promise.reject(failure);
      }
      if (queue.length > 0) {
        const value = queue.shift() as Buffer;
        if (paused) {
          stream.resume();
          paused = false;
        }
        return Promise.resolve({ value, done: false });
      }
      if (ended) {
        return Promise.resolve({ value: undefined, done: true });
      }
      return new Promise((resolve, reject) => {
        waiter = { resolve, reject };
      });
    },
    [Symbol.asyncIterator](): AsyncIterableIterator<Buffer> {
      return iterator;
    },
  };
  return iterator;
}
