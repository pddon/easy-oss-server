"""XML 小工具：命名空间无关的元素读取。"""

import xml.etree.ElementTree as ET
from typing import List


def parse_xml(data) -> ET.Element:
    """解析 XML 字节串；非法 XML 抛出 ValueError。"""
    if isinstance(data, str):
        data = data.encode("utf-8")
    try:
        return ET.fromstring(data)
    except ET.ParseError as exc:
        raise ValueError("XML 解析失败: %s" % exc) from exc


def local_name(tag: str) -> str:
    return tag.rsplit("}", 1)[-1]


def child_text(element: ET.Element, name: str, default: str = "") -> str:
    for child in element:
        if local_name(child.tag) == name:
            return child.text or default
    return default


def iter_all(element: ET.Element, name: str):
    """遍历所有后代元素（忽略命名空间）。"""
    for node in element.iter():
        if local_name(node.tag) == name:
            yield node


def find_all(element: ET.Element, name: str) -> List[ET.Element]:
    return list(iter_all(element, name))
