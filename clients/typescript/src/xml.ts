/**
 * 极简 XML 解析 / 构造工具（零依赖）。
 *
 * 只覆盖本服务与 S3 常用响应所需子集：元素、属性、文本、XML 声明。
 * 不处理 DTD；`<ListBucketResult xmlns="...">` 这类默认命名空间按普通属性忽略，
 * 元素名原样保留，查找时兼容 local name。
 */

export interface XmlNode {
  name: string;
  attrs: Record<string, string>;
  children: XmlNode[];
  text: string;
}

export interface ParsedError {
  code: string;
  message: string;
  resource: string;
  requestId: string;
}

export interface XmlBuilder {
  raw(value: string): XmlBuilder;
  element(name: string, value: string | number): XmlBuilder;
  toString(): string;
}

function createElement(name: string): XmlNode {
  return { name, attrs: {}, children: [], text: '' };
}

function decodeEntities(input: string): string {
  return String(input).replace(/&(#x?[0-9a-fA-F]+|[a-zA-Z]+);/g, (match, entity: string) => {
    switch (entity) {
      case 'lt':
        return '<';
      case 'gt':
        return '>';
      case 'amp':
        return '&';
      case 'quot':
        return '"';
      case 'apos':
        return "'";
      default:
        if (entity[0] === '#') {
          const codePoint =
            entity[1] === 'x' || entity[1] === 'X'
              ? parseInt(entity.slice(2), 16)
              : parseInt(entity.slice(1), 10);
          return Number.isFinite(codePoint) ? String.fromCodePoint(codePoint) : match;
        }
        return match;
    }
  });
}

export function escapeXml(input: string | number): string {
  return String(input)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&apos;');
}

function parseAttrs(raw: string): Record<string, string> {
  const attrs: Record<string, string> = {};
  const attrPattern = /([^\s=/>]+)(?:\s*=\s*("[^"]*"|'[^']*'))?/g;
  let match: RegExpExecArray | null;
  while ((match = attrPattern.exec(raw)) !== null) {
    if (!match[1]) {
      continue;
    }
    const rawValue = match[2] ? match[2].slice(1, -1) : '';
    attrs[match[1]] = decodeEntities(rawValue);
  }
  return attrs;
}

/** 解析 XML 文本，返回 `#document` 根节点。格式非法时抛错。 */
export function parseXml(input: string): XmlNode {
  const source = String(input)
    .replace(/^\uFEFF/, '')
    .replace(/<\?xml[^>]*\?>/gi, '')
    .replace(/<!--[\s\S]*?-->/g, '');

  const documentNode = createElement('#document');
  const stack: XmlNode[] = [documentNode];
  const tagPattern = /<(\/?)([^\s/>]+)((?:\s+[^\s=/>]+(?:\s*=\s*(?:"[^"]*"|'[^']*'))?)*)\s*(\/?)>/g;
  let cursor = 0;
  let match: RegExpExecArray | null;

  while ((match = tagPattern.exec(source)) !== null) {
    const textBefore = source.slice(cursor, match.index);
    if (textBefore.trim()) {
      stack[stack.length - 1].text += decodeEntities(textBefore);
    }
    cursor = tagPattern.lastIndex;

    const closing = match[1] === '/';
    const tagName = match[2];
    const rawAttrs = match[3] || '';
    const selfClosing = match[4] === '/';

    if (closing) {
      if (stack.length === 1 || stack[stack.length - 1].name !== tagName) {
        throw new Error(`Malformed XML: unexpected closing tag </${tagName}>`);
      }
      stack.pop();
      continue;
    }

    const node = createElement(tagName);
    node.attrs = parseAttrs(rawAttrs);
    stack[stack.length - 1].children.push(node);
    if (!selfClosing) {
      stack.push(node);
    }
  }

  const tail = source.slice(cursor);
  if (tail.trim()) {
    stack[stack.length - 1].text += decodeEntities(tail);
  }
  if (stack.length !== 1) {
    throw new Error(`Malformed XML: unclosed element <${stack[stack.length - 1].name}>`);
  }
  return documentNode;
}

export function localName(name: string): string {
  const index = name.indexOf(':');
  return index === -1 ? name : name.slice(index + 1);
}

/** 取文档的第一个元素节点（跳过 #document 包装）。 */
export function rootElement(document: XmlNode | null): XmlNode | null {
  if (!document) {
    return null;
  }
  if (document.name === '#document') {
    return document.children.length > 0 ? document.children[0] : null;
  }
  return document;
}

export function findChild(node: XmlNode | null, name: string): XmlNode | null {
  if (!node) {
    return null;
  }
  for (const child of node.children) {
    if (child.name === name || localName(child.name) === name) {
      return child;
    }
  }
  return null;
}

export function findChildren(node: XmlNode | null, name: string): XmlNode[] {
  if (!node) {
    return [];
  }
  return node.children.filter((child) => child.name === name || localName(child.name) === name);
}

/** 取直接子元素文本；不存在返回 fallback（默认 ''）。 */
export function childText(node: XmlNode | null, name: string, fallback?: string): string {
  const child = findChild(node, name);
  if (!child) {
    return fallback === undefined ? '' : fallback;
  }
  return child.text.trim();
}

export function childNumber(node: XmlNode | null, name: string, fallback: number): number {
  const raw = childText(node, name, '');
  if (raw === '') {
    return fallback;
  }
  const value = Number(raw);
  return Number.isFinite(value) ? value : fallback;
}

/** 把 `<Error>` 响应解析成 { code, message, resource, requestId }；非 Error 返回 null。 */
export function parseErrorXml(input: string): ParsedError | null {
  let root: XmlNode;
  try {
    root = parseXml(input);
  } catch (err) {
    return null;
  }
  const errorNode =
    findChild(root, 'Error') ||
    (root.children.length > 0 && localName(root.children[0].name) === 'Error'
      ? root.children[0]
      : null);
  if (!errorNode) {
    return null;
  }
  const actual = rootElement(errorNode) || errorNode;
  return {
    code: childText(actual, 'Code', 'InternalError') || 'InternalError',
    message: childText(actual, 'Message', '') || 'Request failed',
    resource: childText(actual, 'Resource', ''),
    requestId: childText(actual, 'RequestId', ''),
  };
}

export function buildXml(declaration = true): XmlBuilder {
  const parts: string[] = declaration ? ['<?xml version="1.0" encoding="UTF-8"?>'] : [];
  const builder: XmlBuilder = {
    raw(value: string): XmlBuilder {
      parts.push(value);
      return builder;
    },
    element(name: string, value: string | number): XmlBuilder {
      parts.push(`<${name}>${escapeXml(value)}</${name}>`);
      return builder;
    },
    toString(): string {
      return parts.join('');
    },
  };
  return builder;
}
