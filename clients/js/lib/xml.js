'use strict';

/**
 * 极简 XML 解析 / 构造工具（零依赖）。
 *
 * 只覆盖本服务与 S3 常用响应所需子集：元素、属性、文本、CDATA、XML 声明。
 * 不处理 DTD、命名空间前缀合并；`<ListBucketResult xmlns="...">` 这种默认
 * 命名空间按普通属性忽略，元素名原样保留。
 */

function decodeEntities(input) {
  return String(input).replace(/&(#x?[0-9a-fA-F]+|[a-zA-Z]+);/g, (match, entity) => {
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

function escapeXml(input) {
  return String(input)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&apos;');
}

function createElement(name) {
  return { name, attrs: {}, children: [], text: '' };
}

const TAG_RE = /<([!?][^>]*?|[^>]+?)\s*\/?>|<!--[\s\S]*?-->|<![CDATA[[\s\S]*?]]>/g;

/** 解析 XML 文本，返回根节点。格式非法时抛错。 */
function parseXml(input) {
  const source = String(input)
    .replace(/^\uFEFF/, '')
    .replace(/<\?xml[^>]*\?>/gi, '')
    .replace(/<!--[\s\S]*?-->/g, '');

  const root = createElement('#document');
  const stack = [root];
  const tagPattern = /<(\/?)([^\s/>]+)((?:\s+[^\s=/>]+(?:\s*=\s*(?:"[^"]*"|'[^']*'))?)*)\s*(\/?)>/g;
  let cursor = 0;
  let match;

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
  return root;
}

function parseAttrs(raw) {
  const attrs = {};
  const attrPattern = /([^\s=/>]+)(?:\s*=\s*("[^"]*"|'[^']*'))?/g;
  let match;
  while ((match = attrPattern.exec(raw)) !== null) {
    if (!match[1]) {
      continue;
    }
    let value = match[2] ? match[2].slice(1, -1) : '';
    value = decodeEntities(value);
    attrs[match[1]] = value;
  }
  return attrs;
}

function localName(name) {
  const index = name.indexOf(':');
  return index === -1 ? name : name.slice(index + 1);
}

/** 取文档的第一个元素节点（跳过 #document 包装）。 */
function rootElement(document) {
  if (!document) {
    return null;
  }
  if (document.name === '#document') {
    return document.children.length > 0 ? document.children[0] : null;
  }
  return document;
}

function findChild(node, name) {
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

function findChildren(node, name) {
  if (!node) {
    return [];
  }
  return node.children.filter((child) => child.name === name || localName(child.name) === name);
}

/** 取直接子元素文本；不存在返回 ''。 */
function childText(node, name, fallback) {
  const child = findChild(node, name);
  if (!child) {
    return fallback === undefined ? '' : fallback;
  }
  return child.text.trim();
}

function childNumber(node, name, fallback) {
  const raw = childText(node, name, '');
  if (raw === '') {
    return fallback === undefined ? 0 : fallback;
  }
  const value = Number(raw);
  return Number.isFinite(value) ? value : fallback;
}

/** 把 `<Error>` 响应解析成 { code, message, resource, requestId }；非 Error 返回 null。 */
function parseErrorXml(input) {
  let root;
  try {
    root = parseXml(input);
  } catch (err) {
    return null;
  }
  const errorNode =
    findChild(root, 'Error') ||
    (root.children.length > 0 && localName(root.children[0].name) === 'Error' ? root.children[0] : null);
  if (!errorNode) {
    return null;
  }
  const actual = rootElement(errorNode);
  return {
    code: childText(actual, 'Code', 'InternalError') || 'InternalError',
    message: childText(actual, 'Message', '') || 'Request failed',
    resource: childText(actual, 'Resource', ''),
    requestId: childText(actual, 'RequestId', ''),
  };
}

function buildXml(declaration) {
  const parts = declaration === false ? [] : ['<?xml version="1.0" encoding="UTF-8"?>'];
  return {
    raw(value) {
      parts.push(value);
      return this;
    },
    element(name, value) {
      parts.push(`<${name}>${escapeXml(value)}</${name}>`);
      return this;
    },
    toString() {
      return parts.join('');
    },
  };
}

module.exports = {
  buildXml,
  childNumber,
  childText,
  escapeXml,
  findChild,
  findChildren,
  localName,
  parseErrorXml,
  parseXml,
  rootElement,
};
